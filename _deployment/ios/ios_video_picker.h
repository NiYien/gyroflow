#pragma once

class QObject;
class QUrl;

bool gyroflowIosOpenVideoPicker(QObject *receiver);
bool gyroflowIosShareFile(const QUrl &url);
void gyroflowIosCleanupVideoImports();
