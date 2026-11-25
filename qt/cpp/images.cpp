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

void rv_set_image(rust::Str key, rust::Slice<const uint8_t> pixels, uint32_t width, uint32_t height, bool half) {
    const auto format = half ? QImage::Format_RGBA16FPx4 : QImage::Format_RGBA8888;
    const qsizetype bpl = qsizetype(width) * (half ? 8 : 4);
    QImage img(int(width), int(height), format);
    if (img.isNull() || qsizetype(pixels.size()) < bpl * qsizetype(height)) return;
    for (uint32_t y = 0; y < height; ++y)
        memcpy(img.scanLine(int(y)), pixels.data() + bpl * y, size_t(bpl));
    QMutexLocker l(&lock);
    frames.insert(QString::fromUtf8(key.data(), qsizetype(key.size())), img);
}

void rv_install_images(QQmlApplicationEngine &engine) {
    engine.addImageProvider(QStringLiteral("rv"), new RvImages);
}
