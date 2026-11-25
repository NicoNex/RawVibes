// A window whose swapchain is HDR (extended linear sRGB, scRGB): the photo's HDR rendition is
// shown in it, embedded in the editor with QML's WindowContainer. Qt Quick does not colour-manage
// 2D content, so only the photo lives here; the rest of the interface stays on an SDR surface.
#pragma once

#include <QtQuick/QQuickWindow>
#include <rhi/qrhi.h>

class RvHdrWindow : public QQuickWindow {
    Q_OBJECT
    // The swapchain really is HDR (an SDR screen gets an SDR one), and what 1.0 means on it,
    // in units of SDR white: scene-referred scRGB is 80 nits, display-referred (macOS EDR) is SDR white.
    Q_PROPERTY(bool hdr READ hdr NOTIFY hdrChanged)
    Q_PROPERTY(double white READ white NOTIFY hdrChanged)
public:
    explicit RvHdrWindow(QWindow *parent = nullptr) : QQuickWindow(parent) {
        // Read by the Qt Quick renderer when it creates the swapchain.
        setProperty("_qt_sg_hdr_format", QByteArrayLiteral("scrgb"));
        connect(this, &QQuickWindow::afterRendering, this, &RvHdrWindow::check, Qt::DirectConnection);
    }
    bool hdr() const { return m_hdr; }
    double white() const { return m_white; }
signals:
    void hdrChanged();

private:
    // On the render thread, after each frame: what the swapchain turned out to be. Compared with
    // the render thread's own copy; the GUI thread's (read by QML) is set through the event queue.
    void check() {
        QRhiSwapChain *sc = swapChain();
        if (!sc) return;
        const bool hdr = sc->format() != QRhiSwapChain::SDR;
        const QRhiSwapChainHdrInfo info = sc->hdrInfo();
        const double white = info.luminanceBehavior == QRhiSwapChainHdrInfo::SceneReferred
            ? (info.sdrWhiteLevel > 0 ? info.sdrWhiteLevel : 203.0) / 80.0 : 1.0;
        if (m_seen && hdr == m_rtHdr && qFuzzyCompare(white, m_rtWhite)) return;
        m_seen = true;
        m_rtHdr = hdr;
        m_rtWhite = white;
        QMetaObject::invokeMethod(this, [this, hdr, white] {
            m_hdr = hdr;
            m_white = white;
            emit hdrChanged();
        }, Qt::QueuedConnection);
    }
    bool m_hdr = false;
    double m_white = 203.0 / 80.0;
    // Render thread only.
    bool m_seen = false;
    bool m_rtHdr = false;
    double m_rtWhite = 0;
};
