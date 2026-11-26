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
    // A frame has been rendered, so `hdr` is known: an SDR window steps aside after it.
    Q_PROPERTY(bool probed READ probed NOTIFY hdrChanged)
    // HDR10 (Rec.2020 PQ, `white` in cd/m²) rather than scRGB (`white` a multiplier).
    Q_PROPERTY(bool pq READ pq NOTIFY hdrChanged)
public:
    explicit RvHdrWindow(QWindow *parent = nullptr) : QQuickWindow(parent) {
        // Read by the Qt Quick renderer when it creates the swapchain. HDR10 is what Vulkan drivers on
        // Wayland offer (NVIDIA has no extended sRGB) and D3D too; macOS EDR takes scRGB.
#ifdef Q_OS_MACOS
        setProperty("_qt_sg_hdr_format", QByteArrayLiteral("scrgb"));
#else
        setProperty("_qt_sg_hdr_format", QByteArrayLiteral("hdr10"));
#endif
        connect(this, &QQuickWindow::afterRendering, this, &RvHdrWindow::check, Qt::DirectConnection);
    }
    bool hdr() const { return m_hdr; }
    double white() const { return m_white; }
    bool probed() const { return m_probed; }
    bool pq() const { return m_pq; }
signals:
    void hdrChanged();

private:
    // On the render thread, after each frame: what the swapchain turned out to be. Compared with
    // the render thread's own copy; the GUI thread's (read by QML) is set through the event queue.
    void check() {
        QRhiSwapChain *sc = swapChain();
        if (!sc) return;
        const bool hdr = sc->format() != QRhiSwapChain::SDR;
        const bool pq = sc->format() == QRhiSwapChain::HDR10;
        const QRhiSwapChainHdrInfo info = sc->hdrInfo();
        // SDR white in cd/m² as the system sets it, else BT.2408's 203.
        const double nits = info.luminanceBehavior == QRhiSwapChainHdrInfo::SceneReferred && info.sdrWhiteLevel > 0
            ? info.sdrWhiteLevel : 203.0;
        const double white = pq ? nits
            : info.luminanceBehavior == QRhiSwapChainHdrInfo::SceneReferred ? nits / 80.0 : 1.0;
        if (m_rtProbed && hdr == m_rtHdr && qFuzzyCompare(white, m_rtWhite)) return;
        if (!m_rtProbed)
            qInfo("rawvibes-qt: HDR window swapchain format %d", int(sc->format()));
        m_rtProbed = true;
        m_rtHdr = hdr;
        m_rtWhite = white;
        QMetaObject::invokeMethod(this, [this, hdr, pq, white] {
            m_probed = true;
            m_pq = pq;
            m_hdr = hdr;
            m_white = white;
            emit hdrChanged();
        }, Qt::QueuedConnection);
    }
    bool m_hdr = false;
    bool m_probed = false;
    bool m_pq = false;
    double m_white = 203.0 / 80.0;
    // Render thread only.
    bool m_rtProbed = false;
    bool m_rtHdr = false;
    double m_rtWhite = 203.0 / 80.0;
};
