#include "MDKPlayer.h"
#include <map>
#include <string>
#include <thread>
#include <QTimer>
#include <QStringList>
#include <QJsonArray>
#include <QGuiApplication>
#if __has_include(<QX11Info>)
#include <QX11Info>
#endif

#include "mdk/Player.h"
#include "mdk/VideoFrame.h"
using namespace mdk;

void printMd(const std::map<std::string, std::string> &md) {
    for (auto &x : md) {
        qDebug2("printMd") << QString::fromStdString(x.first) << " = " << QString::fromStdString(x.second);
    }
}
std::string toStdString(const QString &str) { return std::string(qUtf8Printable(str), str.size()); }

MDKPlayer::MDKPlayer() {
#ifdef QX11INFO_X11_H
    SetGlobalOption("X11Display", QX11Info::display());
    qDebug("X11 display: %p", QX11Info::display());
#elif (QT_FEATURE_xcb + 0 == 1) && (QT_VERSION >= QT_VERSION_CHECK(6, 2, 0))
    const auto x = qGuiApp->nativeInterface<QNativeInterface::QX11Application>();
    if (x) {
        const auto xdisp = x->display();
        SetGlobalOption("X11Display", xdisp);
        qDebug("X11 display: %p", xdisp);
    }
#endif
}

std::vector<std::string> MDKPlayer::defaultDecoderList() const {
    QString overrideDecoders = QString(qgetenv("MDK_DECODERS")).trimmed();

    if (!overrideDecoders.isEmpty()) {
        std::vector<std::string> vec;
        for (const auto &x : overrideDecoders.split(",")) {
            vec.push_back(toStdString(x));
        }
        return vec;
    } else {
        return {
    #if (__APPLE__+0)
        "VT:duration=0",
    #elif (__ANDROID__+0)
        // image=1 delivers decoded frames through AImageReader (AHardwareBuffer)
        // instead of the SurfaceTexture handoff. The SurfaceTexture path lacks
        // fence synchronization between the hardware decoder writing a buffer
        // and GL sampling it (observed on Xiaomi/HyperOS): the decoder fills
        // macroblock rows top-to-bottom, so the bottom of the frame is the last
        // to complete and shows as high-frequency tearing/wobble during
        // playback. AImageReader keeps zero-copy and hardware decode.
        "AMediaCodec:java=0:copy=0:surface=1:async=0:image=1",
    #elif (_WIN32+0)
        // "MFT:d3d=11",
        //"CUDA",
        //"NVDEC",
        //"CUVID",
        // Slice threading avoids the extra hardware surfaces that frame threading allocates.
        // Formats D3D11/DXVA cannot decode are switched to "FFmpeg" in setUrl before the decoder opens.
        "D3D11:threads=0:thread_type=slice:sw_fallback=1",
        "DXVA:threads=0:thread_type=slice:sw_fallback=1",
    #elif (__linux__+0)
        "CUDA",
        "VDPAU",
        "VAAPI:sw_fallback=1",
    #endif
        "BRAW:gpu=auto:copy=1:scale=1920x1080",
    #if defined(Q_OS_MACOS) && defined(Q_PROCESSOR_ARM_64)
        // The upstream async Metal path avoids the legacy R3D decoder's
        // quarter-resolution stripes and expensive half-resolution workaround.
        // Copy its 16-bit output because the plugin's native texture mapping
        // does not provide a valid Metal pixel format to MDK's renderer.
        "R3D:gpu=metal:decompress=async:format=rgb48le:copy=1:scale=1920x1080",
    #elif (__APPLE__+0)
        // Keep the existing aligned half-resolution path on other Apple targets.
        "R3D:gpu=auto:scale=3840x2160",
    #else
        "R3D:gpu=auto:scale=1920x1080",
    #endif
        "FFmpeg"};
    }
}

QString MDKPlayer::defaultVideoDecoders() const {
    QStringList names;
    for (const auto &name : defaultDecoderList()) names.append(QString::fromStdString(name));
    return names.join(',');
}

void MDKPlayer::initPlayer() {
    m_player = std::make_unique<mdk::Player>();
    m_metadata = QJsonObject();
    m_shuttingDown = false;

    m_player->setDecoders(mdk::MediaType::Video, defaultDecoderList());
#if (_WIN32+0)
    m_autoSoftwareDecoders = QString(qgetenv("MDK_DECODERS")).trimmed().isEmpty();
#endif

    if (m_item && m_node && m_window) {
        setupPlayer();
        if (m_size.width() > 0 && m_size.height() > 0) {
            m_syncNext = true;
        }
    }
}
void MDKPlayer::destroyPlayer() {
    m_shuttingDown = true; // Signal render thread to stop before any cleanup
    m_playbackRequested.store(false, std::memory_order_release);
    m_videoLoaded = false;
    m_firstFrameLoaded = false;
    if (m_connectionBeforeRendering) QObject::disconnect(m_connectionBeforeRendering);
    if (m_connectionScreenChanged) QObject::disconnect(m_connectionScreenChanged);

    if (m_player) {
        stop();
        m_player->setRenderCallback([](void *) {});
        m_player->onMediaStatusChanged([](mdk::MediaStatus) -> bool { return false; });
        m_player->onStateChanged([](mdk::State) {});
        m_player->onEvent([](const mdk::MediaEvent &) -> bool { return false; });
        m_player->onFrame<mdk::VideoFrame>([](mdk::VideoFrame&, int) -> int { return 0; });
        auto ptr = m_player.release();
        const bool deleteInBackground = m_isR3dFormat;
        QTimer::singleShot(1000, [ptr, deleteInBackground] {
            if (deleteInBackground) {
                std::thread([ptr] { delete ptr; }).detach();
            } else {
                delete ptr;
            }
        }); // delete later
    }
}

MDKPlayer::~MDKPlayer() {
    m_shuttingDown = true;
    m_videoLoaded = false;
    m_firstFrameLoaded = false;
    m_processPixels = nullptr;
    m_processTexture = nullptr;
    m_readyForProcessing = nullptr;
    m_item = nullptr;
    m_window = nullptr;

    if (m_userDataDestructor && m_userData) { m_userDataDestructor(m_userData); m_userData = nullptr; }
    if (m_userData2Destructor && m_userData2) { m_userData2Destructor(m_userData2); m_userData2 = nullptr; }

    destroyPlayer();
}

void MDKPlayer::setProperty(const QString &key, const QString &value) {
#if (_WIN32+0)
    if (key == "video.decoders") {
        m_autoSoftwareDecoders = QString(qgetenv("MDK_DECODERS")).trimmed().isEmpty() && value == defaultVideoDecoders();
    }
#endif
    if (m_player) {
        m_player->setProperty(toStdString(key), toStdString(value));
    }
}

void MDKPlayer::setDefaultProperty(const QString &key, const QString &value) {
    setProperty(key, value);
    m_defaultProperties.insert(key, value);
}

void MDKPlayer::setSourceAspectRatio(bool enabled) {
    m_sourceAspectRatio = enabled;
    applySourceAspectRatio();
    forceRedraw();
    if (m_item) m_item->update();
}

void MDKPlayer::applySourceAspectRatio() {
    if (!m_player) return;
    if (!m_sourceAspectRatio) {
        m_player->setAspectRatio(mdk::KeepAspectRatio);
        return;
    }
    // The processing surface already follows the source pixel dimensions.
    // Fill it without applying display SAR or introducing letterbox padding.
    m_player->setAspectRatio(mdk::IgnoreAspectRatio);
}

void MDKPlayer::setToneCurve(const uint16_t *data, size_t len) {
    std::lock_guard<std::mutex> lock(m_toneCurveMutex);
    if (!data || len != TONE_CURVE_LEN) {
        if (m_toneCurve) qDebug2("setToneCurve") << "cleared";
        m_toneCurve.reset();
        m_toneCurveProbed = false;
        return;
    }
    m_toneCurve = std::make_shared<const std::vector<uint16_t>>(data, data + len);
    m_toneCurveProbed = false;
    qDebug2("setToneCurve") << "installed" << len << "entries";
}

#if (_WIN32+0)
// FFmpeg offers D3D11/DXVA output only for these formats. Anything else falls back to software
// inside the hardware decoder, which keeps its slice-only threading; single-slice streams then
// decode on one core.
static bool hardwareDecoderSupports(const mdk::VideoCodecParameters &codec) {
    const std::string name = codec.codec ? codec.codec : "";
    const std::string format = codec.format_name ? codec.format_name : "";
    if (format.empty()) return true;
    if (name == "h264") return format == "yuv420p" || format == "yuvj420p";
    if (name == "hevc") return format == "yuv420p" || format == "yuvj420p" || format == "yuv420p10le";
    return true;
}
#endif

void MDKPlayer::setUrl(const QUrl &url, const QString &customDecoder) {
    m_overrideFps = 0.0;
    if (!m_item || !m_window || !m_node) {
        m_pendingUrl = url;
        m_pendingCustomDecoder = customDecoder;
        return;
    }
    if (url.toString().contains("http://") || url.toString().contains("https://")) {
        m_isHttp = true;
    }
    const bool isR3dFormat = customDecoder.startsWith("R3D:");
    destroyPlayer();
    m_isR3dFormat = isR3dFormat;
    initPlayer();

    QString additionalUrl;
    if (!customDecoder.isEmpty()) {
        qDebug2("setUrl") << "MDK decoder:" << customDecoder;
        if (customDecoder.startsWith("FFmpeg:avformat_options=")) {
            additionalUrl = "?mdkopt=avformat&" + customDecoder.mid(24);
        } else if (customDecoder.startsWith("headers:")) {
            qDebug2("setUrl") << "Setting custom HTTP headers:" << customDecoder.mid(8);
            m_player->setProperty("avio.headers", customDecoder.mid(8).toStdString());
        }
    }

    QString path = url.toEncoded() + additionalUrl;

    // Workaround because avdevice:// is not a valid URL according to QUrl
    if (path.startsWith("http://avdevice/")) {
        path = "avdevice://" + path.mid(strlen("http://avdevice/")).replace("%20", " ");
    } else {
        if (url.scheme() == "file") {
            path = url.toLocalFile() + additionalUrl;
        } else if (path.contains(' ')) {
            path.replace(' ', "%20");
        }
    }
    qDebug2("setUrl") << "Final url:" << path;
    m_player->setMedia(qUtf8Printable(path));
#if (_WIN32+0)
    auto player = m_player.get();
    // Called once the stream info is known and before the video decoder is created.
    m_player->prepare(0, [this, player](int64_t position, bool *) {
        if (position < 0 || !m_autoSoftwareDecoders.load()) return true;
        const auto &info = player->mediaInfo();
        if (!info.video.empty() && !hardwareDecoderSupports(info.video[0].codec)) {
            player->setDecoders(mdk::MediaType::Video, {"FFmpeg"});
            qDebug2("setUrl") << "Software decoder selected for" << info.video[0].codec.codec << info.video[0].codec.format_name;
        }
        return true;
    });
#else
    m_player->prepare();
#endif
}

void MDKPlayer::setBackgroundColor(const QColor &color) {
    m_bgColor = color;
    if (m_player)
        m_player->setBackgroundColor(m_bgColor.redF(), m_bgColor.greenF(), m_bgColor.blueF(), m_bgColor.alphaF());
    forceRedraw();
}

void MDKPlayer::setMuted(bool v) {
    if (m_player)
        m_player->setMute(v);
}
bool MDKPlayer::getMuted() { return m_player? m_player->isMute() : false; }

void MDKPlayer::setVolume(float v) { if (m_player) m_player->setVolume(v); }
float MDKPlayer::getVolume() { return m_player? m_player->volume() : 0.0; }

void MDKPlayer::setupNode(QSGImageNode *node, QQuickItem *item) {
    m_node = node;
    m_item = item;
    m_window = item? item->window() : nullptr;
    if (!m_window) return;
    node->setOwnsTexture(true);
    if (!m_pendingUrl.isEmpty()) {
        setUrl(m_pendingUrl, m_pendingCustomDecoder);
        m_pendingUrl = QUrl();
    } else if (!m_player) {
        initPlayer();
    }
}

void MDKPlayer::setProcessPixelsCallback(ProcessPixelsCb &&cb) {
    m_processPixels = cb;
}
void MDKPlayer::setProcessTextureCallback(ProcessTextureCb &&cb) {
    m_processTexture = cb;
}
void MDKPlayer::setReadyForProcessingCallback(ReadyForProcessingCb &&cb) {
    m_readyForProcessing = cb;
}

void MDKPlayer::setupPlayer() {
    auto player = m_player.get();
#if defined(Q_OS_MACOS) && defined(Q_PROCESSOR_ARM_64)
    // OpenAL rejects queued audio buffers after RAW seeks and loops on macOS.
    if (m_isR3dFormat) m_player->setAudioBackends({"AudioQueue"});
#endif
    m_player->setRenderCallback([this, player](void *) {
        if (m_player.get() != player) return;
        QMetaObject::invokeMethod(m_item, "update");
    });
    m_player->setProperty("continue_at_end", "1");
    if (!m_isHttp) {
        m_player->setBufferRange(0);
    }
    for (auto it = m_defaultProperties.constBegin(); it != m_defaultProperties.constEnd(); ++it) {
        setProperty(it.key(), it.value());
    }
    m_player->onEvent([this, player](const mdk::MediaEvent &evt) -> bool {
        if (m_player.get() != player) return false;
        if (evt.category == "metadata") {
            auto md = player->mediaInfo();
            QJsonObject obj;
            for (const auto &x : getMediaInfo(md)) {
                obj.insert(QString::fromUtf8(x.first.c_str(), x.first.size()), QString::fromUtf8(x.second.c_str(), x.second.size()));
            }
            m_metadata = obj;
        }
        if (evt.detail == "size") {
            QMetaObject::invokeMethod(m_item, "metadataLoaded", Qt::QueuedConnection, Q_ARG(QJsonObject, m_metadata));

            m_firstFrameLoaded = true;
        }
        qDebug2("m_player->onEvent") << QString::fromUtf8(evt.category.c_str(), evt.category.size()) << QString::fromUtf8(evt.detail.c_str(), evt.detail.size());
        return true;
    });

    // Tone-curve filter. MDK invokes this before handing the frame to the
    // renderer, which is the last point where the samples are still wider than
    // the RGBA8 render target. With no curve installed - i.e. every load that
    // is not a CinemaDNG carrying a LinearizationTable - it returns immediately
    // and the frame is delivered byte for byte as before.
    m_player->onFrame<mdk::VideoFrame>([this, player](mdk::VideoFrame &frame, int) -> int {
        if (m_player.get() != player) return 0;

        std::shared_ptr<const std::vector<uint16_t>> curve;
        {
            std::lock_guard<std::mutex> lock(m_toneCurveMutex);
            curve = m_toneCurve;
        }
        if (!curve) return 0;
        if (!frame || frame.timestamp() == mdk::TimestampEOS) return 0;

        // The decoded frame is a CFA layout MDK has no PixelFormat name for, so
        // to() is the only supported way to reach its samples. RGBA64 keeps all
        // 16 bits through the demosaic, which is the whole point of doing this
        // here rather than after the renderer.
        auto wide = frame.to(mdk::PixelFormat::RGBA64);
        if (!wide) {
            qDebug2("toneCurve") << "to(RGBA64) failed; frame left untouched";
            return 0;
        }
        const int w = wide.width();
        const int h = wide.height();
        const int srcStride = wide.bytesPerLine(0);
        const uint8_t *src = wide.bufferData(0);
        const int dstStride = w * 8; // RGBA64 = 4 channels * 2 bytes
        if (w <= 0 || h <= 0 || !src || srcStride < dstStride) {
            qDebug2("toneCurve") << "unexpected RGBA64 geometry" << w << h << srcStride;
            return 0;
        }

        if (!m_toneCurveProbed.exchange(true)) {
            // One-shot diagnostic: what the decoder handed over, and whether the
            // widening really preserved 16 bits. Alpha is skipped - it is a
            // constant 65535 and would swamp the range. A frame that was only
            // ever 8-bit has every sample either v*257 or v<<8, so a single
            // sample that is neither proves the data is genuinely 16-bit.
            uint16_t mn = 0xFFFF, mx = 0;
            bool not257 = false, not256 = false;
            for (int y = 0; y < h; ++y) {
                const auto *s = reinterpret_cast<const uint16_t *>(src + size_t(y) * size_t(srcStride));
                for (int x = 0; x < w; ++x, s += 4) {
                    for (int c = 0; c < 3; ++c) {
                        // Explicit comparisons: <windows.h> defines min/max as
                        // macros, which breaks std::min/std::max on MSVC.
                        if (s[c] < mn) mn = s[c];
                        if (s[c] > mx) mx = s[c];
                        if (s[c] % 257 != 0) not257 = true;
                        if (s[c] % 256 != 0) not256 = true;
                    }
                }
            }
            qDebug2("toneCurve") << "in fmt" << int(frame.format()) << "planes" << frame.planeCount()
                                 << "-> RGBA64" << w << "x" << h << "RGB range" << mn << ".." << mx
                                 << ((not257 && not256) ? "(true 16-bit)" : "(WARNING: 8-bit derived)");
        }

        std::vector<uint8_t> dst(size_t(dstStride) * size_t(h));
        const uint16_t *lut = curve->data();
        for (int y = 0; y < h; ++y) {
            const auto *s = reinterpret_cast<const uint16_t *>(src + size_t(y) * size_t(srcStride));
            auto *d = reinterpret_cast<uint16_t *>(dst.data() + size_t(y) * size_t(dstStride));
            for (int x = 0; x < w; ++x, s += 4, d += 4) {
                d[0] = lut[s[0]];
                d[1] = lut[s[1]];
                d[2] = lut[s[2]];
                d[3] = s[3]; // alpha carries no scene light
            }
        }

        // Allocate and copy in one step: this constructor path allocates a
        // contiguous buffer and copies `planes` into it, so `dst` may die with
        // this scope. (addBuffer() on a freshly constructed frame is rejected -
        // it expects the plane to already exist.)
        int strides[1] = { dstStride };
        const uint8_t *planes[1] = { dst.data() };
        mdk::VideoFrame out(w, h, mdk::PixelFormat::RGBA64, strides, planes);
        if (!out || !out.bufferData(0)) {
            qDebug2("toneCurve") << "could not build the curved frame; leaving it untouched";
            return 0;
        }
        out.setTimestamp(frame.timestamp());
        frame = std::move(out);
        return 0; // one frame in, one frame out
    });

    m_player->setBackgroundColor(m_bgColor.redF(), m_bgColor.greenF(), m_bgColor.blueF(), m_bgColor.alphaF());
    m_player->setPlaybackRate(m_playbackRate);

    m_player->onStateChanged([this, player](mdk::State state) {
        if (m_player.get() != player) return;
        // qDebug2("m_player->onStateChanged") <<
        //     QString(state == mdk::State::NotRunning?  "NotRunning"  : "") +
        //     QString(state == mdk::State::Running?     "Running"     : "") +
        //     QString(state == mdk::State::Paused?      "Paused"      : "");

        QMetaObject::invokeMethod(m_item, "stateChanged", Q_ARG(int, int(state)));
    });

    m_player->onMediaStatusChanged([this, player](mdk::MediaStatus status) -> bool {
        if (m_player.get() != player) return false;

        if (status & mdk::MediaStatus::Buffering) {
            QMetaObject::invokeMethod(m_item, "setBuffering", Q_ARG(bool, true));
        } else if ((status & mdk::MediaStatus::Buffered) && !(status & mdk::MediaStatus::Seeking)) {
            QJsonArray ranges;
            auto br = player->bufferedTimeRanges();
            for (const auto &r : br) {
                QJsonObject obj;
                obj.insert("start", QJsonValue(double(r.start)));
                obj.insert("end",   QJsonValue(double(r.end)));
                ranges.append(obj);
            }
            QMetaObject::invokeMethod(m_item, "setBufferedRanges", Q_ARG(QJsonArray, ranges));
            QMetaObject::invokeMethod(m_item, "setBuffering", Q_ARG(bool, false));
        }
        // qDebug2("m_player->onMediaStatusChanged") <<
        //     QString(status & mdk::MediaStatus::Unloaded?  "Unloaded | "  : "") +
        //     QString(status & mdk::MediaStatus::Loading?   "Loading | "   : "") +
        //     QString(status & mdk::MediaStatus::Loaded?    "Loaded | "    : "") +
        //     QString(status & mdk::MediaStatus::Prepared?  "Prepared | "  : "") +
        //     QString(status & mdk::MediaStatus::Stalled?   "Stalled | "   : "") +
        //     QString(status & mdk::MediaStatus::Buffering? "Buffering | " : "") +
        //     QString(status & mdk::MediaStatus::Buffered?  "Buffered | "  : "") +
        //     QString(status & mdk::MediaStatus::End?       "End | "       : "") +
        //     QString(status & mdk::MediaStatus::Seeking?   "Seeking | "   : "") +
        //     QString(status & mdk::MediaStatus::Invalid?   "Invalid | "   : "");

        bool expectedLoaded = false;
        if ((status & mdk::MediaStatus::Loaded)
            && (status & mdk::MediaStatus::Prepared)
            && m_videoLoaded.compare_exchange_strong(expectedLoaded, true, std::memory_order_acq_rel)) {
            auto md = player->mediaInfo();

            /*QJsonObject obj;
            for (const auto &x : getMediaInfo(md)) {
                obj.insert(QString::fromStdString(x.first), QString::fromStdString(x.second));
            }
            QMetaObject::invokeMethod(m_item, "metadataLoaded", Q_ARG(QJsonObject, obj));*/

            if (!md.video.empty()) {
                auto v = md.video[0];
                if (!v.frames && v.duration > 0 && v.codec.frame_rate > 0) {
                    v.frames = (double(v.duration) / 1000.0) * v.codec.frame_rate;
                }
                m_fps = v.codec.frame_rate;
                if (m_sourceAspectRatio) applySourceAspectRatio();
                double fps = m_fps;
                m_duration = v.duration;
                if (m_overrideFps > 0.0) {
                    m_duration *= m_fps / m_overrideFps;
                    fps = m_overrideFps;
                }

                QMetaObject::invokeMethod(m_item, "videoLoaded", Q_ARG(double, m_duration), Q_ARG(qlonglong, v.frames), Q_ARG(double, fps), Q_ARG(uint, v.codec.width), Q_ARG(uint, v.codec.height));
            } else if (!md.audio.empty()) {
                auto a = md.audio[0];
                m_duration = a.duration;
                m_fps = 0;

                QMetaObject::invokeMethod(m_item, "videoLoaded", Q_ARG(double, m_duration), Q_ARG(qlonglong, 0), Q_ARG(double, 0), Q_ARG(uint, 0), Q_ARG(uint, 0));
                QMetaObject::invokeMethod(m_item, "metadataLoaded", Qt::QueuedConnection, Q_ARG(QJsonObject, m_metadata));
                QMetaObject::invokeMethod(m_item, "update");
                m_firstFrameLoaded = true;
            }
            player->setLoop(9999999);

            if (!m_connectionBeforeRendering)
                m_connectionBeforeRendering = QObject::connect(m_window, &QQuickWindow::beforeRendering, [this] { this->windowBeforeRendering(); });
            if (!m_connectionScreenChanged)
                m_connectionScreenChanged = QObject::connect(m_window, &QQuickWindow::screenChanged, [this](QScreen *) { m_item->update(); });
        }
        if (status & mdk::MediaStatus::Invalid) {
            QMetaObject::invokeMethod(m_item, "videoLoaded", Q_ARG(double, 0), Q_ARG(qlonglong, 0), Q_ARG(double, 0), Q_ARG(uint, 0), Q_ARG(uint, 0));
            QMetaObject::invokeMethod(m_item, "metadataLoaded", Q_ARG(QJsonObject, QJsonObject()));
        }

        return true;
    });

    /*m_player->onFrame<mdk::VideoFrame>([this](mdk::VideoFrame &frame, int track) -> int {
        //QMetaObject::invokeMethod(m_item, "frameRendered", Q_ARG(double, frame.timestamp() * 1000.0));
        // qDebug2("m_player->onFrame") << "frame" << track << frame.timestamp() << (int)frame.format() << frame.planeCount() << frame.bufferData(0);
        return 0;
    });*/

    if (m_size.width() > 0 && m_size.height() > 0) {
        m_player->setVideoSurfaceSize(m_size.width(), m_size.height());
        m_syncNext = true;
    }
    forceRedraw();
}

void MDKPlayer::windowBeforeRendering() {
    // QElapsedTimer t; t.start();
    if (m_shuttingDown.load()) return;
    if (!m_item || !m_window) return;
    if (!m_videoLoaded.load()) return;

    // Capture player pointer locally to avoid TOCTOU race with destroyPlayer() on GUI thread.
    // destroyPlayer() may call m_player.release() concurrently, making m_player null.
    // The released player is kept alive for 1 second via QTimer, so our local pointer remains valid.
    auto player = m_player.get();
    if (!player) return;

    // Audio-only file: no video frames to render, just report position
    if (m_fps == 0) {
        double ts_ms = double(player->position());
        QMetaObject::invokeMethod(m_item, "frameRendered", Q_ARG(double, ts_ms), Q_ARG(int, 0));
        return;
    }

    // Don't render if sync() hasn't set up the render API for the current player yet
    if (m_syncNext) return;

    if (!m_playbackRequested.load(std::memory_order_acquire) && m_renderedPosition == m_playerPosition && m_renderedReturnCount++ > 100) {
        return;
    }
    if (m_readyForProcessing && !m_readyForProcessing(m_item)) return;

    auto context = static_cast<QSGDefaultRenderContext *>(QQuickItemPrivate::get(m_item)->sceneGraphRenderContext());
    auto cb = context->currentFrameCommandBuffer();

    bool doRenderPass = m_rt && m_window->rendererInterface()->graphicsApi() != QSGRendererInterface::MetalRhi
#if QT_VERSION >= QT_VERSION_CHECK(6, 6, 0)
        && m_window->rendererInterface()->graphicsApi() != QSGRendererInterface::Direct3D12
#endif
    ;

    if (doRenderPass) {
        QRhiResourceUpdateBatch *u = context->rhi()->nextResourceUpdateBatch();
        QRhiTextureRenderTarget *rt = m_rt.get();
#if (__ANDROID__+0)
        // m_rt preserves its color contents on Android, so the very first pass
        // after the texture was (re)created goes through the clearing target to
        // initialise the allocation. Both target the same texture, so which one
        // opens the pass only decides whether the contents survive.
        if (m_needsInitialClear && m_rtClear) {
            rt = m_rtClear.get();
            m_needsInitialClear = false;
        }
#endif
        cb->beginPass(rt, QColor(Qt::black), { 1.0f, 0 }, u, QRhiCommandBuffer::ExternalContent);
    }

    cb->beginExternal();
    double timestamp = player->renderVideo();
    cb->endExternal();

    if (doRenderPass) {
        cb->endPass();
    }

    if (timestamp < 0) {
        return;
    }

    m_playerPosition = timestamp * 1000;

    double fps = m_fps;
    if (m_overrideFps > 0.0) {
        timestamp *= m_fps / m_overrideFps;
        fps = m_overrideFps;
    }

    int frame = std::ceil(std::round(timestamp * fps * 100.0) / 100.0);

    bool processed = false;
    if (m_firstFrameLoaded.load()) {
        if (!m_videoLoaded.load() || m_shuttingDown.load()) return;
        if (m_processTexture && m_texture) {
            uint64_t backend_id = 0;
            uint64_t ptr1 = m_texture->nativeTexture().object;
            uint64_t ptr2 = 0;
            uint64_t ptr3 = 0;
            uint64_t ptr4 = 0;
            uint64_t ptr5 = 0;

            QSGRendererInterface *rif = m_window->rendererInterface();
            switch (rif->graphicsApi()) {
                case QSGRendererInterface::OpenGLRhi: {
                    backend_id = 1;
                    ptr2 = uint64_t(rif->getResource(m_window, QSGRendererInterface::OpenGLContextResource));
                } break;
                case QSGRendererInterface::MetalRhi: {
                    backend_id = 2;
                    ptr2 = uint64_t(rif->getResource(m_window, QSGRendererInterface::DeviceResource));
                    ptr3 = uint64_t(rif->getResource(m_window, QSGRendererInterface::CommandQueueResource));
                } break;
                case QSGRendererInterface::Direct3D11Rhi: {
                    backend_id = 3;
                    ptr2 = uint64_t(rif->getResource(m_window, QSGRendererInterface::DeviceResource));
                    ptr3 = uint64_t(rif->getResource(m_window, QSGRendererInterface::DeviceContextResource));
                } break;
                case QSGRendererInterface::VulkanRhi: {
                    context->rhi()->finish();
                    backend_id = 4;
                    ptr2 = uint64_t(rif->getResource(m_window, QSGRendererInterface::DeviceResource));
                    ptr3 = uint64_t(rif->getResource(m_window, QSGRendererInterface::CommandListResource));
                    ptr4 = uint64_t(rif->getResource(m_window, QSGRendererInterface::PhysicalDeviceResource));
#if __has_include(<vulkan/vulkan_core.h>)
#  if QT_CONFIG(vulkan)
                    auto inst = reinterpret_cast<QVulkanInstance *>(rif->getResource(m_window, QSGRendererInterface::VulkanInstanceResource));
                    ptr5 = inst? uint64_t(inst->vkInstance()) : 0;
#  endif
#endif
                } break;
                default: break;
            }

#if (_WIN32+0)
    // On DirectX we need to wait until MDK finishes rendering the video before we do any further processing with it
    if (rif->graphicsApi() == QSGRendererInterface::Direct3D11Rhi) {
        auto dev = (ID3D11Device *)       rif->getResource(m_window, QSGRendererInterface::DeviceResource);
        auto ctx = (ID3D11DeviceContext *)rif->getResource(m_window, QSGRendererInterface::DeviceContextResource);

        ID3D11Device5 *dev5 = nullptr;
        ID3D11DeviceContext4 *ctx4 = nullptr;
        if (dev->QueryInterface(IID_ID3D11Device5, (void**)&dev5) == S_OK) {
            if (ctx->QueryInterface(IID_ID3D11DeviceContext4, (void**)&ctx4) == S_OK) {
                if (!m_fence) {
                    if (dev5->CreateFence(m_fenceValue, D3D11_FENCE_FLAG_NONE, __uuidof(ID3D11Fence), (void**)&m_fence) == S_OK) {
                        m_event = CreateEventW(NULL, FALSE, FALSE, NULL);
                    }
                }
                if (m_fence && m_event) {
                    ctx4->Signal(m_fence, ++m_fenceValue);
                    m_fence->SetEventOnCompletion(m_fenceValue, m_event);
                    WaitForSingleObject(m_event, 5000);
                }
                ctx4->Release();
            }
            dev5->Release();
        }
    }
#endif
            processed = m_processTexture(m_item, frame, timestamp * 1000.0, m_size.width(), m_size.height(), backend_id, ptr1, ptr2, ptr3, ptr4, ptr5);

            // -------------- Readback workaround --------------
            // if (processed && rif->graphicsApi() == QSGRendererInterface::Direct3D11Rhi) {
            //     if (!m_readbackResult) m_readbackResult = new QRhiReadbackResult();
            //     QRhiResourceUpdateBatch *u = context->rhi()->nextResourceUpdateBatch();
            //     u->readBackTexture({ m_workaroundTexture }, m_readbackResult);
            //     cb->resourceUpdate(u);
            // }
            // -------------- Readback workaround --------------

            if (processed) m_renderFailCounter = 0;
            else           m_renderFailCounter++;
        }

        if (!processed && m_processPixels) {
            if (!m_processTexture || m_renderFailCounter > 10) {
                auto img = toImage();
                if (!m_videoLoaded.load() || m_shuttingDown.load()) return;

                const auto img2 = m_processPixels(m_item, frame, timestamp * 1000.0, img);
                if (!img2.isNull() && img2.constBits()) {
                    fromImage(img2);
                }
            }
        }
    }

    if (m_renderedPosition != m_playerPosition)
        m_renderedReturnCount = 0;

    m_renderedPosition = m_playerPosition;
    // printf("render: %.3f\n", double(t.nsecsElapsed()) / 1000000.0);

    QMetaObject::invokeMethod(m_item, "frameRendered", Q_ARG(double, timestamp * 1000.0), Q_ARG(int, frame));
}

void MDKPlayer::sync(QSGImageNode *node, QSize newSize, QQuickItem *item, bool force) {
    if (m_shuttingDown.load()) return;
    if (!m_item || !m_window || !item || m_item != item) return;
    if (!node) return;
    if (m_syncNext) {
        force = true;
        m_syncNext = false;
    }
    if (!m_player) { m_size = newSize; return; }
    if (!force && node->texture() && newSize == m_size)
        return;

    if (newSize.width() < 32 || newSize.height() < 32)
        newSize = QSize(32, 32);

    m_size = newSize;

    releaseResources();
    auto tex = createTexture(m_player.get(), m_size);
    if (!tex)
        return;
    qDebug2("MDKPlayer::sync") << "created texture" << tex << m_size;
    QMetaObject::invokeMethod(m_item, "surfaceSizeUpdated", Q_ARG(uint, m_size.width()), Q_ARG(uint, m_size.height()));
    node->setTexture(tex);
    node->setOwnsTexture(true);
    node->setTextureCoordinatesTransform(m_tx); // MUST set when texture() is available
    node->setFiltering(QSGTexture::Linear);
    node->setRect(0, 0, m_item->width(), m_item->height());
    m_player->setVideoSurfaceSize(m_size.width(), m_size.height());
    if (m_sourceAspectRatio) applySourceAspectRatio();
}

void MDKPlayer::play() {
    if (!m_videoLoaded || !m_player) return;
    const int64_t pos = m_playerPosition;
    m_playbackRequested.store(true, std::memory_order_release);
#if defined(Q_OS_MACOS) && defined(Q_PROCESSOR_ARM_64)
    // Queue the re-anchor while paused. Starting the async decoder first lets
    // its old decode chain race the seek and submit duplicate/out-of-order frames.
    if (m_isR3dFormat) m_player->seek(pos, mdk::SeekFlag::FromStart);
#endif
    m_player->set(mdk::PlaybackState::Playing);
    forceRedraw();
#if !(defined(Q_OS_MACOS) && defined(Q_PROCESSOR_ARM_64))
    if (m_isR3dFormat) m_player->seek(pos, mdk::SeekFlag::FromStart);
#endif
}
void MDKPlayer::pause() {
    if (!m_videoLoaded || !m_player) return;
    m_playbackRequested.store(false, std::memory_order_release);
    m_player->set(mdk::PlaybackState::Paused);
    forceRedraw();
}
void MDKPlayer::stop() {
    if (!m_videoLoaded || !m_player) return;
    m_playbackRequested.store(false, std::memory_order_release);
    m_player->set(mdk::PlaybackState::Stopped);
    m_player->waitFor(mdk::PlaybackState::Stopped);
}
void MDKPlayer::setFrameRate(float fps) {
    if (!m_player) return;
    m_player->setFrameRate(fps);
    m_overrideFps = fps;
}

void MDKPlayer::seekToTimestamp(float timestampMs, bool exact) {
    if (!m_videoLoaded || !m_player) return;

    m_player->seek(timestampMs, (exact? mdk::SeekFlag::FromStart : mdk::SeekFlag::FromStart | mdk::SeekFlag::KeyFrame) | mdk::SeekFlag::InCache);
    forceRedraw();
}

void MDKPlayer::seekToFrameDelta(int64_t frameDelta) {
    if (!m_videoLoaded || !m_player) return;

    m_player->seek(frameDelta, mdk::SeekFlag::FromNow | mdk::SeekFlag::Frame | mdk::SeekFlag::InCache);
    forceRedraw();
}

void MDKPlayer::seekToFrame(int64_t frame, int64_t currentFrame, bool exact) {
    if (!m_videoLoaded || !m_player) return;

    auto md = m_player->mediaInfo();
    if (!md.video.empty()) {
        auto v = md.video[0];
        seekToTimestamp((frame / v.codec.frame_rate) * 1000.0, exact);
    }
    forceRedraw();
}

void MDKPlayer::setPlaybackRate(float rate) { m_playbackRate = rate; if (m_player) m_player->setPlaybackRate(rate); }
float MDKPlayer::playbackRate() { return m_player? m_player->playbackRate() : m_playbackRate; }

void MDKPlayer::setPlaybackRange(int64_t from_ms, int64_t to_ms) {
    if (m_overrideFps > 0.0) {
        from_ms /= m_fps / m_overrideFps;
        to_ms   /= m_fps / m_overrideFps;
    }
    if (m_player) m_player->setRange(from_ms, to_ms);
}

void MDKPlayer::setRotation(int v) {
    if (!m_videoLoaded || !m_player) return;

    m_player->rotate(v);
    forceRedraw();
}
int MDKPlayer::getRotation() {
    if (!m_videoLoaded || !m_player) return 0;

    auto md = m_player->mediaInfo();
    if (!md.video.empty()) {
        return md.video[0].rotation;
    }
    return 0;
}

void MDKPlayer::stopProcessingPlayer(uint64_t id) {
    if (m_processingPlayers.find(id) == m_processingPlayers.end()) return;
    m_processingPlayers[id]->set(mdk::PlaybackState::Stopped);
    m_processingPlayers[id]->waitFor(mdk::PlaybackState::Stopped);
    //m_processingPlayers.erase(id);
}

void MDKPlayer::initProcessingPlayer(uint64_t id, uint64_t width, uint64_t height, bool yuv, std::string custom_decoder, const std::vector<std::pair<uint64_t, uint64_t>> &ranges, VideoProcessCb &&cb) { // ms
    m_processingPlayers[id] = std::make_unique<mdk::Player>();
    auto player = m_processingPlayers[id].get();
    if (!custom_decoder.empty()) {
        player->setDecoders(MediaType::Video, { custom_decoder });
    } else {
        player->setDecoders(MediaType::Video, { "FFmpeg", "BRAW:gpu=auto", "R3D:gpu=auto" });
    }


    player->setMedia(m_player? m_player->url() : qUtf8Printable(m_pendingUrl.toLocalFile()));

    player->setDecoders(MediaType::Audio, { });
    player->setMute(true);
    player->onSync([] { return DBL_MAX; });

    if (ranges.empty()) {
        const_cast<std::vector<std::pair<uint64_t, uint64_t>> &>(ranges).push_back({ 0, UINT64_MAX });
    }

    auto range_id = std::make_shared<uint>(0);
    auto finished = std::make_shared<std::atomic_bool>(false);
    auto finish = [cb, finished](int result) {
        if (!finished->exchange(true)) {
            cb(result, -1.0, 0, 0, 0, 0, 0, 0, 0, 0, 0);
        }
    };

    player->onFrame<mdk::VideoFrame>([cb, finish, width, height, id, range_id = std::move(range_id), finished, yuv, ranges, this](mdk::VideoFrame &v, int) -> int {
        if (*finished) return 0;
        if (!v || v.timestamp() == mdk::TimestampEOS) { // AOT frame(1st frame, seek end 1st frame) is not valid, but format is valid. eof frame format is invalid
            finish(-1);
            return 0;
        }
        if (!v.format()) {
            printf("error occured!\n");
            return 0;
        }

        auto timestamp_ms = v.timestamp() * 1000.0;

        auto md = m_processingPlayers[id]->mediaInfo();
        if (!md.video.empty()) {
            auto vmd = md.video[0];

            auto frame_num = std::ceil(std::round(v.timestamp() * vmd.codec.frame_rate * 100) / 100.0);

            if (width == 0) const_cast<uint64_t&>(width) = v.width();
            if (height == 0) const_cast<uint64_t&>(height) = v.height();

            auto format = yuv? mdk::PixelFormat::YUV420P : mdk::PixelFormat::RGBA;
            if (!strcmp(md.format, "r3d")) format = mdk::PixelFormat::BGRA;

            /*switch (v.format()) {
                case mdk::PixelFormat::YUV420P:     qDebug() << "YUV420P";     break;
                case mdk::PixelFormat::NV12:        qDebug() << "NV12";        break;
                case mdk::PixelFormat::YUV422P:     qDebug() << "YUV422P";     break;
                case mdk::PixelFormat::YUV444P:     qDebug() << "YUV444P";     break;
                case mdk::PixelFormat::P010LE:      qDebug() << "P010LE";      break;
                case mdk::PixelFormat::P016LE:      qDebug() << "P016LE";      break;
                case mdk::PixelFormat::YUV420P10LE: qDebug() << "YUV420P10LE"; break;
                case mdk::PixelFormat::UYVY422:     qDebug() << "UYVY422";     break;
                case mdk::PixelFormat::RGB24:       qDebug() << "RGB24";       break;
                case mdk::PixelFormat::RGBA:        qDebug() << "RGBA";        break;
                case mdk::PixelFormat::RGBX:        qDebug() << "RGBX";        break;
                case mdk::PixelFormat::BGRA:        qDebug() << "BGRA";        break;
                case mdk::PixelFormat::BGRX:        qDebug() << "BGRX";        break;
                case mdk::PixelFormat::RGB565LE:    qDebug() << "RGB565LE";    break;
                case mdk::PixelFormat::RGB48LE:     qDebug() << "RGB48LE";     break;
                case mdk::PixelFormat::GBRP:        qDebug() << "GBRP";        break;
                case mdk::PixelFormat::GBRP10LE:    qDebug() << "GBRP10LE";    break;
                case mdk::PixelFormat::XYZ12LE:     qDebug() << "XYZ12LE";     break;
                case mdk::PixelFormat::YUVA420P:    qDebug() << "YUVA420P";    break;
                case mdk::PixelFormat::BC1:         qDebug() << "BC1";         break;
                case mdk::PixelFormat::BC3:         qDebug() << "BC3";         break;
                case mdk::PixelFormat::RGBA64:      qDebug() << "RGBA64";      break;
                case mdk::PixelFormat::BGRA64:      qDebug() << "BGRA64";      break;
                case mdk::PixelFormat::RGBP16:      qDebug() << "RGBP16";      break;
                case mdk::PixelFormat::RGBPF32:     qDebug() << "RGBPF32";     break;
                case mdk::PixelFormat::BGRAF32:     qDebug() << "BGRAF32";     break;
            }*/

            auto vscaled = v.to(format, width, height);
            auto ptr = vscaled.bufferData();
            auto ptr_size = vscaled.bytesPerLine() * vscaled.height();

            if (!cb(frame_num, timestamp_ms, vscaled.width(), vscaled.height(), vmd.codec.width, vmd.codec.height, vmd.codec.frame_rate, vmd.duration, vmd.frames, ptr, ptr_size)) {
                // If cb returns false - stop the processing
                m_processingPlayers[id]->set(mdk::PlaybackState::Paused);
                m_processingPlayers[id]->waitFor(mdk::PlaybackState::Paused);
                finish(-1);
                return 0;
            }
        }

        if (timestamp_ms >= ranges[*range_id].second) {
            if (*range_id + 1 < ranges.size()) {
                *range_id += 1;
                m_processingPlayers[id]->seek(ranges[*range_id].first, mdk::SeekFlag::FromStart);
                return 0;
            }
            m_processingPlayers[id]->set(mdk::PlaybackState::Paused);
            m_processingPlayers[id]->waitFor(mdk::PlaybackState::Paused);
            finish(-1);
            return 0;
        }

        return 0;
    });
    player->setVideoSurfaceSize(64, 64);

    player->prepare(ranges[0].first, [finish](int64_t position, bool *) {
        if (position >= 0) return true;
        // Opening can fail without delivering any frame, including an EOS frame.
        // Wake the processing caller so it can stop and release this player.
        qWarning() << "MDK processing prepare failed:" << position;
        finish(-2);
        return false;
    });
    player->set(State::Running);
}

std::map<std::string, std::string> MDKPlayer::getMediaInfo(const MediaInfo &mi) {
    std::map<std::string, std::string> ret;
    ret["start_time"] = std::to_string(mi.start_time);
    ret["bit_rate"]   = std::to_string(mi.bit_rate);
    ret["size"]       = std::to_string(mi.size);
    ret["format"]     = std::string(mi.format);
    ret["streams"]    = std::to_string(mi.streams);
    for (const auto &x : mi.metadata) {
        ret["metadata." + x.first] = x.second;
    }
    int i = 0;
    for (const auto &v : mi.video) {
        std::string key = "stream.video[" + std::to_string(i) + "].";
        ret[key + "index"] = std::to_string(v.index);
        ret[key + "start_time"] = std::to_string(v.start_time);
        ret[key + "duration"] = std::to_string(m_duration);
        ret[key + "frames"] = std::to_string(v.frames);
        ret[key + "rotation"] = std::to_string(v.rotation);
        for (const auto &x : v.metadata) {
            ret[key + "metadata." + x.first] = x.second;
        }

        ret[key + "codec.name"]         = std::string(v.codec.codec);
        ret[key + "codec.tag"]          = std::to_string(v.codec.codec_tag);
        //ret[key + "codec.extra_data"] = std::to_string(v.codec.extra_data, v.codec.extra_data_size);
        ret[key + "codec.bit_rate"]     = std::to_string(v.codec.bit_rate);
        ret[key + "codec.profile"]      = std::to_string(v.codec.profile);
        ret[key + "codec.level"]        = std::to_string(v.codec.level);
        ret[key + "codec.frame_rate"]   = std::to_string(m_overrideFps > 0.0? m_overrideFps : v.codec.frame_rate);
        ret[key + "codec.format"]       = std::to_string(v.codec.format);
        ret[key + "codec.format_name"]  = std::string(v.codec.format_name);
        ret[key + "codec.width"]        = std::to_string(v.codec.width);
        ret[key + "codec.height"]       = std::to_string(v.codec.height);
        ret[key + "codec.b_frames"]     = std::to_string(v.codec.b_frames);

        ++i;
    }
    i = 0;
    for (const auto &a : mi.audio) {
        std::string key = "stream.audio[" + std::to_string(i) + "].";
        ret[key + "index"] = std::to_string(a.index);
        ret[key + "start_time"] = std::to_string(a.start_time);
        ret[key + "duration"] = std::to_string(a.duration);
        ret[key + "frames"] = std::to_string(a.frames);
        for (const auto &x : a.metadata) {
            ret[key + "metadata." + x.first] = x.second;
        }

        ret[key + "codec.name"]            = std::string(a.codec.codec);
        ret[key + "codec.tag"]             = std::to_string(a.codec.codec_tag);
        //ret[key + "codec.extra_data"]    = std::to_string(a.codec.extra_data, a.codec.extra_data_size);
        ret[key + "codec.bit_rate"]        = std::to_string(a.codec.bit_rate);
        ret[key + "codec.profile"]         = std::to_string(a.codec.profile);
        ret[key + "codec.level"]           = std::to_string(a.codec.level);
        ret[key + "codec.frame_rate"]      = std::to_string(a.codec.frame_rate);
        ret[key + "codec.is_float"]        = a.codec.is_float?    "true" : "false";
        ret[key + "codec.is_unsigned"]     = a.codec.is_unsigned? "true" : "false";
        ret[key + "codec.is_planar"]       = a.codec.is_planar?   "true" : "false";
        ret[key + "codec.raw_sample_size"] = std::to_string(a.codec.raw_sample_size);
        ret[key + "codec.channels"]        = std::to_string(a.codec.channels);
        ret[key + "codec.sample_rate"]     = std::to_string(a.codec.sample_rate);
        ret[key + "codec.block_align"]     = std::to_string(a.codec.block_align);
        ret[key + "codec.frame_size"]      = std::to_string(a.codec.frame_size);

        ++i;
    }
    return ret;
}

QSGDefaultRenderContext *MDKPlayer::rhiContext() {
    return static_cast<QSGDefaultRenderContext *>(QQuickItemPrivate::get(m_item)->sceneGraphRenderContext());
}
QRhiTexture *MDKPlayer::rhiTexture() {
    return m_texture;
}
QRhiTextureRenderTarget *MDKPlayer::rhiRenderTarget() {
    return m_rt.get();
}
QRhiRenderPassDescriptor *MDKPlayer::rhiRenderPassDescriptor() {
    return m_rtRp.get();
}
QQuickWindow *MDKPlayer::qmlWindow() {
    return m_window;
}
QQuickItem *MDKPlayer::qmlItem() {
    return m_item;
}
QSize MDKPlayer::textureSize() {
    return m_size;
}
QMatrix4x4 MDKPlayer::textureMatrix() {
    return m_proj;
}
void *MDKPlayer::userData() const {
    return m_userData;
}
void MDKPlayer::setUserData(void *ptr) {
    m_userData = ptr;
}
void MDKPlayer::setUserDataDestructor(std::function<void(void *)> &&cb) {
    m_userDataDestructor = cb;
}

void *MDKPlayer::userData2() const {
    return m_userData2;
}
void MDKPlayer::setUserData2(void *ptr) {
    m_userData2 = ptr;
}
void MDKPlayer::setUserData2Destructor(std::function<void(void *)> &&cb) {
    m_userData2Destructor = cb;
}
