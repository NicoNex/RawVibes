// The QML image provider: Rust hands it frames by key ("photo", "roll/3"…), QML reads them as image://rv/<key>/<generation>.
#pragma once

#include <QtGui/QImage>
#include <QtQml/QQmlApplicationEngine>
#include <QtQuick/QQuickImageProvider>
#include <QtCore/QHash>
#include <QtCore/QMutex>
#include "rust/cxx.h"

class RvImages : public QQuickImageProvider {
public:
    RvImages() : QQuickImageProvider(QQuickImageProvider::Image) {}
    QImage requestImage(const QString &id, QSize *size, const QSize &requested) override;
};

// Stores an RGBA8 (or RGBA16F when `half`) frame under `key`.
void rv_set_image(rust::Str key, rust::Slice<const uint8_t> pixels, uint32_t width, uint32_t height, bool half);
void rv_install_images(QQmlApplicationEngine &engine);
