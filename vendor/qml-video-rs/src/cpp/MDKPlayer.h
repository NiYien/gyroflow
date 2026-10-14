#ifndef MDK_PLAYER_H
#define MDK_PLAYER_H

#include <QtQuick/QQuickItem>
#include <QtQuick/QQuickWindow>
#include <QtQuick/QSGImageNode>
#include <QtCore/QJsonObject>
#include <QtCore/QHash>
#include <future>
#include <chrono>
#include <queue>
#include <atomic>
#include <array>
#include <functional>
#include <memory>
#include <mutex>
#include <vector>

#include "VideoTextureNode.h"

typedef std::function<bool(QQuickItem *item, uint32_t frame, double timestamp, uint32_t width, uint32_t height, uint32_t backend_id, uint64_t ptr1, uint64_t ptr2, uint64_t ptr3, uint64_t ptr4, uint64_t ptr5)> ProcessTextureCb;
typedef std::function<QImage(QQuickItem *item, uint32_t frame, double timestamp, const QImage &img)> ProcessPixelsCb;
typedef std::function<bool(QQuickItem *item)> ReadyForProcessingCb;
// Negative frame numbers are terminal: -1 means finished, -2 means prepare failed.
typedef std::function<bool(int32_t frame, double timestamp, uint32_t width, uint32_t height, uint32_t org_width, uint32_t org_height, double fps, double duration_ms, uint32_t frame_count, const uint8_t *bits, uint64_t bitsSize)> VideoProcessCb;

namespace mdk { class Player; }

class MDKPlayer : public VideoTextureNodePriv {
public:
    MDKPlayer();
    void initPlayer();
    void destroyPlayer();

    ~MDKPlayer();

    void setUrl(const QUrl &url, const QString &customDecoder);
    void setProperty(const QString &key, const QString &value);
    void setDefaultProperty(const QString &key, const QString &value);
    QString defaultVideoDecoders() const;
    void setSourceAspectRatio(bool enabled);

    // Tone curve applied to decoded frames before they reach the renderer.
    //
    // CinemaDNG decodes to scene-linear samples: ffmpeg honours the file's
    // LinearizationTable, which undoes the perceptually uniform log encoding the
    // camera applied. Rendering that straight into the RGBA8 texture leaves ~10
    // distinct levels with most pixels at zero. The curve maps the samples back
    // onto a uniform range while they are still 16-bit, so the 8-bit render
    // target stops being the bottleneck and does not need to change.
    //
    // `len` must be TONE_CURVE_LEN. Any other length - including 0 - clears the
    // curve, which is how every non-DNG load keeps its existing behaviour.
    static constexpr size_t TONE_CURVE_LEN = 65536;
    void setToneCurve(const uint16_t *data, size_t len);

    void setBackgroundColor(const QColor &color);

    void setMuted(bool v);
    bool getMuted();

    void setVolume(float v);
    float getVolume();

    inline QColor getBackgroundColor() { return m_bgColor; }

    void setupNode(QSGImageNode *node, QQuickItem *item);
    void setProcessPixelsCallback(ProcessPixelsCb &&cb);
    void setProcessTextureCallback(ProcessTextureCb &&cb);
    void setReadyForProcessingCallback(ReadyForProcessingCb &&cb);

    void setupPlayer();

    void windowBeforeRendering();

    void sync(QSGImageNode *node, QSize newSize, QQuickItem *item, bool force = false);
    void forceRedraw() { m_renderedPosition = -1; m_playerPosition = 0; m_renderedReturnCount = 0; }

    void play();
    void pause();
    void stop();

    void seekToTimestamp(float timestampMs, bool exact = true);
    void seekToFrame(int64_t frame, int64_t currentFrame, bool exact = true);
    void seekToFrameDelta(int64_t frameDelta);

    void setFrameRate(float fps);

    void setPlaybackRate(float rate);
    float playbackRate();

    void setPlaybackRange(int64_t from_ms, int64_t to_ms);

    void setRotation(int v);
    int getRotation();

    void initProcessingPlayer(uint64_t id, uint64_t width, uint64_t height, bool yuv, std::string custom_decoder, const std::vector<std::pair<uint64_t, uint64_t>> &ranges, VideoProcessCb &&cb);
    void stopProcessingPlayer(uint64_t id);

    std::map<std::string, std::string> getMediaInfo(const MediaInfo &mi);

    QSGDefaultRenderContext *rhiContext();
    QRhiTexture *rhiTexture();
    QRhiTextureRenderTarget *rhiRenderTarget();
    QRhiRenderPassDescriptor *rhiRenderPassDescriptor();
    QQuickWindow *qmlWindow();
    QQuickItem *qmlItem();
    QSize textureSize();
    QMatrix4x4 textureMatrix();

    void *userData() const;
    void setUserData(void *ptr);
    void setUserDataDestructor(std::function<void(void *)> &&cb);

    void *userData2() const;
    void setUserData2(void *ptr);
    void setUserData2Destructor(std::function<void(void *)> &&cb);

private:
#if (_WIN32+0)
    struct D3D11FailureLog {
        std::chrono::steady_clock::time_point next{};
        uint64_t count{0};
        uint32_t result{0};
    };
    // Only accessed by the render thread, including across player reloads.
    std::array<D3D11FailureLog, 7> m_d3d11Failures{};
    void logD3D11Failure(size_t index, const char *stage, uint32_t result, ID3D11Device *device, DWORD win32Error = 0);
#endif
    void applySourceAspectRatio();
    std::atomic<bool> m_sourceAspectRatio{false};
    std::vector<std::string> defaultDecoderList() const;
    // True while the preview uses the built-in Windows decoder list, which may be
    // narrowed to software decoding once the stream format is known.
    std::atomic<bool> m_autoSoftwareDecoders{false};

    QMetaObject::Connection m_connectionBeforeRendering;
    QMetaObject::Connection m_connectionScreenChanged;

    void *m_userData{nullptr};
    std::function<void(void *)> m_userDataDestructor;

    void *m_userData2{nullptr};
    std::function<void(void *)> m_userData2Destructor;

    ProcessPixelsCb m_processPixels;
    ProcessTextureCb m_processTexture;
    ReadyForProcessingCb m_readyForProcessing;

    std::unique_ptr<mdk::Player> m_player;
    struct SourceVideoInfo {
        std::atomic<bool> ready{false};
        std::mutex mutex;
        double duration{0.0}, fps{0.0};
        qlonglong frames{0};
        uint width{0}, height{0};
    };
    void captureSourceVideoInfo(const std::shared_ptr<SourceVideoInfo> &source, const MediaInfo &info);
    std::shared_ptr<SourceVideoInfo> m_sourceVideoInfo;
    std::map<uint64_t, std::unique_ptr<mdk::Player>> m_processingPlayers;

    std::atomic<bool> m_videoLoaded{false};
    std::atomic<bool> m_firstFrameLoaded{false};

    int m_renderFailCounter{10};

    int64_t m_renderedPosition{-1};
    int64_t m_renderedReturnCount{0};
    double m_fps{0.0};
    double m_overrideFps{0.0};
    double m_duration{0.0};
    float m_playbackRate{1.0};
    bool m_syncNext{false};
    bool m_isHttp{false};
    bool m_isR3dFormat{false};
    std::atomic<bool> m_playbackRequested{false};
    int64_t m_playerPosition{0};

    QJsonObject m_metadata;

    QSGImageNode *m_node{nullptr};
    QColor m_bgColor;
    QUrl m_pendingUrl;
    QString m_pendingCustomDecoder;
    QHash<QString, QString> m_defaultProperties;
    std::atomic<bool> m_shuttingDown{false};

    // Guarded because the onFrame filter runs on a decoder thread while
    // setToneCurve() is called from the GUI thread on load.
    std::mutex m_toneCurveMutex;
    std::shared_ptr<const std::vector<uint16_t>> m_toneCurve;
    std::atomic<bool> m_toneCurveProbed{false};
};

// Simple wrapper class to workaround class alignment issues when using it from Rust
class MDKPlayerWrapper {
public:
    MDKPlayerWrapper() { mdkplayer = new MDKPlayer(); }
    ~MDKPlayerWrapper() { delete mdkplayer; }
    MDKPlayer *mdkplayer{nullptr};
};

#endif
