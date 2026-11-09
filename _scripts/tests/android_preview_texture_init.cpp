// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 Gyroflow contributors

#define NOMINMAX
#include <QGuiApplication>
#include <QQuickItem>
#include <QQuickWindow>
#include <QTimer>
#include <QJsonObject>
#include <atomic>
#if defined(_WIN32)
#include <d3d11.h>
#include <d3d12.h>
#endif
#include <mdk/Player.h>
#include <mdk/VideoFrame.h>
#include <mdk/RenderAPI.h>

// Pass a solid-red YUV420 video (Y=81, U=90, V=240) as the sole argument.
// Exercise the production Android render-target branch with the host Qt RHI.
// Qt and MDK headers above keep their native host ABI.
#define __ANDROID__ 1
#include "VideoTextureNode.cpp"
#include "MDKPlayer.cpp"
#undef __ANDROID__

class PreviewProbe : public QQuickItem {
    Q_OBJECT
public:
    explicit PreviewProbe(QQuickWindow *window, const QString &media) : QQuickItem(window->contentItem()) {
        setFlag(ItemHasContents);
        setSize(QSizeF(160, 90));
        player.setReadyForProcessingCallback([this](QQuickItem *) { return !blocked; });
        player.setProcessTextureCallback([this](QQuickItem *, uint32_t, double, uint32_t, uint32_t,
                                               uint32_t, uint64_t, uint64_t, uint64_t, uint64_t, uint64_t) {
            ++processed;
            return true;
        });
        player.setUrl(QUrl::fromLocalFile(media), QStringLiteral("FFmpeg"));
        connect(window, &QQuickWindow::beforeRendering, window, [this] {
            // Seed each fresh allocation, making undefined initial pixels deterministic.
            // Do not consume the production initial-clear flag.
            if (player.m_needsInitialClear && seeded != player.rhiTexture()) {
                seeded = player.rhiTexture();
                auto *context = player.rhiContext();
                auto *cb = context->currentFrameCommandBuffer();
                cb->beginPass(player.m_rtClear.get(), QColor(Qt::magenta), {1.0f, 0});
                cb->endPass();
                ++allocations;
            }
        }, Qt::DirectConnection);
        connect(window, &QQuickWindow::afterRendering, window, [this] { inspect(); }, Qt::DirectConnection);
    }

    Q_INVOKABLE void videoLoaded(double, qlonglong, double, uint width, uint height) {
        loaded = width > 0 && height > 0;
        update();
    }
    Q_INVOKABLE void metadataLoaded(QJsonObject) {}
    Q_INVOKABLE void surfaceSizeUpdated(uint, uint) {}
    Q_INVOKABLE void setBuffering(bool) {}
    Q_INVOKABLE void setBufferedRanges(QJsonArray) {}
    Q_INVOKABLE void stateChanged(int) {}
    Q_INVOKABLE void frameRendered(double, int) {}

    MDKPlayer player;

protected:
    QSGNode *updatePaintNode(QSGNode *oldNode, UpdatePaintNodeData *) override {
        auto *node = static_cast<QSGImageNode *>(oldNode);
        if (!node) {
            node = window()->createImageNode();
            player.setupNode(node, this);
        }
        player.sync(node, size().toSize(), this);
        return node;
    }

private:
    void finish(bool passed, const QString &detail) {
        if (finished) return;
        finished = true;
        qInfo().noquote() << (passed ? "TEXTURE_INIT_PASS" : "TEXTURE_INIT_FAIL") << detail;
        QMetaObject::invokeMethod(qApp, [passed] { QCoreApplication::exit(passed ? 0 : 2); }, Qt::QueuedConnection);
    }

    void inspect() {
        if (finished || !loaded || !player.rhiTexture()) return;
        const auto image = player.toImage();
        if (image.isNull()) return;
        if (stage == 3 && image.size() != QSize(128, 72)) return;
        const auto color = image.pixelColor(image.width() / 2, image.height() / 2);
        if (stage == 0 || stage == 3) {
            // Wait for a repaint after the Loaded callback and initial node setup.
            if (++waitFrames < 3) return;
            if (player.m_needsInitialClear || color != QColor(Qt::black) || processed != priorProcessed) {
                finish(false, QString("blocked stage=%1 pending_clear=%2 rgba=%3 processed=%4")
                    .arg(stage).arg(player.m_needsInitialClear).arg(color.name(QColor::HexArgb)).arg(processed));
                return;
            }
            qInfo() << "BLOCKED_TEXTURE_BLACK" << stage << image.size() << "allocations=" << allocations;
            stage = stage == 0 ? 1 : 4;
            blocked = false;
        } else if (stage == 1 || stage == 4) {
            if (processed == priorProcessed) return;
            // The fixture is a solid red video, independent of the renderer's clear color.
            if (color.red() < 200 || color.green() > 40 || color.blue() > 40) {
                finish(false, "decoded fixture did not replace the initial black texture: " + color.name());
                return;
            }
            qInfo() << "DECODED_FRAME_RED" << stage << image.size();
            if (stage == 4) {
                finish(true, "blocked first frame, retained contents, resize, and decoded-frame recovery");
                return;
            }
            blocked = true;
            priorProcessed = processed;
            stage = 2;
            waitFrames = 0;
        } else if (stage == 2) {
            if (color.red() < 200 || color.green() > 40 || color.blue() > 40 || processed != priorProcessed) {
                finish(false, "a later blocked repaint changed the retained decoded frame");
                return;
            }
            if (++waitFrames < 3) return;
            qInfo() << "BLOCKED_REPAINT_RETAINED_RED";
            stage = 3;
            waitFrames = 0;
            seeded = nullptr;
            QMetaObject::invokeMethod(this, [this] { setSize(QSizeF(128, 72)); update(); }, Qt::QueuedConnection);
        }
    }

    std::atomic<bool> loaded{false};
    bool blocked{true};
    bool finished{false};
    int stage{0}, waitFrames{0}, allocations{0}, processed{0}, priorProcessed{0};
    QRhiTexture *seeded{nullptr};
};

int main(int argc, char **argv) {
    QGuiApplication app(argc, argv);
    if (app.arguments().size() != 2) return 3;
    qputenv("MDK_DECODERS", "FFmpeg");
    mdk::SetGlobalOption("log", "Warning");
    QQuickWindow::setGraphicsApi(QSGRendererInterface::OpenGL);
    QQuickWindow window;
    window.setTitle(QStringLiteral("Android texture initialization regression"));
    window.resize(160, 90);
    auto *probe = new PreviewProbe(&window, app.arguments()[1]);
    QTimer repaint;
    QObject::connect(&repaint, &QTimer::timeout, &window, &QQuickWindow::update);
    repaint.start(20);
    QTimer::singleShot(15000, &app, [] { qCritical() << "TEXTURE_INIT_FAIL timeout"; QCoreApplication::exit(4); });
    window.show();
    QTimer::singleShot(0, &window, [&window] { window.hide(); window.show(); });
    const auto result = app.exec();
    window.hide();
    delete probe;
    return result;
}

#include "android_preview_texture_init.moc"
