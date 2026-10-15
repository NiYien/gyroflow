#pragma once

class QObject;
class QUrl;
class QString;

bool gyroflowIosOpenVideoPicker(QObject *receiver);
bool gyroflowIosShareFile(const QUrl &url);
void gyroflowIosCleanupVideoImports();
QString gyroflowIosSaveVideo(const QString &path);
