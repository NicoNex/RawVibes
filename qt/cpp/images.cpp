#include "images.h"

static QMutex lock;
static QHash<QString, QImage> frames;

QImage RvImages::requestImage(const QString &id, QSize *size, const QSize &requested) {
    // The generation after the last '/' only busts QML's cache.
    const QString key = id.left(id.lastIndexOf('/'));
    QImage img;
    {
        QMutexLocker l(&lock);
        img = frames.value(key);
    }
    if (size) *size = img.size();
    if (requested.isValid() && !img.isNull() && requested != img.size())
        return img.scaled(requested, Qt::KeepAspectRatio, Qt::SmoothTransformation);
    return img;
}

void rv_set_image(rust::Str key, rust::Vec<uint8_t> pixels, uint32_t width, uint32_t height, bool half) {
    const auto format = half ? QImage::Format_RGBA16FPx4 : QImage::Format_RGBA8888;
    const qsizetype bpl = qsizetype(width) * (half ? 8 : 4);
    if (width == 0 || height == 0 || qsizetype(pixels.size()) < bpl * qsizetype(height)) return;
    // The image wraps the Rust buffer and frees it with its last copy.
    auto *owned = new rust::Vec<uint8_t>(std::move(pixels));
    QImage img(owned->data(), int(width), int(height), bpl, format,
               [](void *p) { delete static_cast<rust::Vec<uint8_t> *>(p); }, owned);
    QMutexLocker l(&lock);
    frames.insert(QString::fromUtf8(key.data(), qsizetype(key.size())), img);
}

void rv_drop(rust::Str key) {
    QMutexLocker l(&lock);
    frames.remove(QString::fromUtf8(key.data(), qsizetype(key.size())));
}

void rv_install_images(QQmlApplicationEngine &engine) {
    engine.addImageProvider(QStringLiteral("rv"), new RvImages);
}
