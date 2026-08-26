// SPDX-License-Identifier: GPL-3.0-or-later

#import <Photos/Photos.h>
#import <PhotosUI/PhotosUI.h>
#import <UIKit/UIKit.h>
#import <UniformTypeIdentifiers/UniformTypeIdentifiers.h>
#import <objc/runtime.h>

#include <QtCore/QDir>
#include <QtCore/QMetaObject>
#include <QtCore/QObject>
#include <QtCore/QPointer>
#include <QtCore/QStandardPaths>
#include <QtCore/QString>
#include <QtCore/QStringList>
#include <QtCore/QUrl>

#include "ios_video_picker.h"

namespace {

bool pickerActive = false;
char delegateAssociationKey;

QString fromNSString(NSString *value)
{
    return value ? QString::fromUtf8(value.UTF8String) : QString();
}

NSString *toNSString(const QString &value)
{
    const QByteArray utf8 = value.toUtf8();
    return [NSString stringWithUTF8String:utf8.constData()];
}

QString importRootPath()
{
    return QDir(QStandardPaths::writableLocation(QStandardPaths::CacheLocation))
        .filePath(QStringLiteral("ios-photo-imports"));
}

UIWindow *activeWindow()
{
    UIApplication *application = UIApplication.sharedApplication;
    UIWindow *fallback = nil;
    for (UIScene *scene in application.connectedScenes) {
        if (![scene isKindOfClass:UIWindowScene.class])
            continue;
        UIWindowScene *windowScene = static_cast<UIWindowScene *>(scene);
        if (windowScene.activationState != UISceneActivationStateForegroundActive)
            continue;
        for (UIWindow *window in windowScene.windows) {
            if (window.isKeyWindow)
                return window;
            if (!fallback && !window.hidden)
                fallback = window;
        }
    }
    return fallback;
}

UIViewController *topViewController(UIViewController *controller)
{
    while (controller) {
        UIViewController *presented = controller.presentedViewController;
        if (presented && !presented.isBeingDismissed) {
            controller = presented;
            continue;
        }
        if ([controller isKindOfClass:UINavigationController.class]) {
            controller = static_cast<UINavigationController *>(controller).visibleViewController;
            continue;
        }
        if ([controller isKindOfClass:UITabBarController.class]) {
            controller = static_cast<UITabBarController *>(controller).selectedViewController;
            continue;
        }
        break;
    }
    return controller;
}

NSString *videoTypeIdentifier(NSItemProvider *provider)
{
    for (NSString *identifier in provider.registeredTypeIdentifiers) {
        UTType *type = [UTType typeWithIdentifier:identifier];
        if (type && [type conformsToType:UTTypeMovie])
            return identifier;
    }
    return [provider hasItemConformingToTypeIdentifier:UTTypeMovie.identifier]
        ? UTTypeMovie.identifier
        : nil;
}

NSString *destinationFilename(NSItemProvider *provider, NSURL *sourceUrl, NSString *typeIdentifier)
{
    NSString *filename = provider.suggestedName.length
        ? provider.suggestedName.lastPathComponent
        : sourceUrl.lastPathComponent;
    UTType *type = [UTType typeWithIdentifier:typeIdentifier];
    NSString *extension = type.preferredFilenameExtension;
    if (!filename.length)
        filename = extension.length ? [@"video" stringByAppendingPathExtension:extension] : @"video.mov";
    else if (!filename.pathExtension.length && extension.length)
        filename = [filename stringByAppendingPathExtension:extension];
    return filename;
}

void invokeCancelled(const QPointer<QObject> &receiver)
{
    if (receiver)
        QMetaObject::invokeMethod(receiver.data(), "catch_picker_cancelled", Qt::QueuedConnection);
}

} // namespace

@interface GyroflowVideoImportSession : NSObject {
@public
    QPointer<QObject> receiver;
    NSMutableArray *paths;
    NSMutableArray *errors;
    NSString *directory;
    NSInteger remaining;
}
- (instancetype)initWithReceiver:(QObject *)receiver
                            count:(NSInteger)count
                        directory:(NSString *)directory;
- (void)finishItemAtIndex:(NSUInteger)index path:(NSString *)path error:(NSString *)error;
- (void)deliver;
@end

@implementation GyroflowVideoImportSession

- (instancetype)initWithReceiver:(QObject *)receiverObject
                            count:(NSInteger)count
                        directory:(NSString *)directoryPath
{
    self = [super init];
    if (self) {
        receiver = receiverObject;
        remaining = count;
        directory = [directoryPath copy];
        paths = [[NSMutableArray alloc] initWithCapacity:count];
        errors = [[NSMutableArray alloc] initWithCapacity:count];
        for (NSInteger i = 0; i < count; ++i) {
            [paths addObject:NSNull.null];
            [errors addObject:NSNull.null];
        }
    }
    return self;
}

- (void)dealloc
{
    [paths release];
    [errors release];
    [directory release];
    [super dealloc];
}

- (void)finishItemAtIndex:(NSUInteger)index path:(NSString *)path error:(NSString *)error
{
    BOOL finished = NO;
    @synchronized(self) {
        if (path)
            [paths replaceObjectAtIndex:index withObject:path];
        if (error)
            [errors replaceObjectAtIndex:index withObject:error];
        --remaining;
        finished = remaining == 0;
    }
    if (finished) {
        dispatch_async(dispatch_get_main_queue(), ^{
            [self deliver];
        });
    }
}

- (void)deliver
{
    QStringList selectedUrls;
    NSMutableArray *errorMessages = [NSMutableArray array];
    @synchronized(self) {
        for (id value in paths) {
            if (value != NSNull.null) {
                const QUrl url = QUrl::fromLocalFile(fromNSString(static_cast<NSString *>(value)));
                selectedUrls.append(url.toString(QUrl::FullyEncoded));
            }
        }
        for (id value in errors) {
            if (value != NSNull.null)
                [errorMessages addObject:value];
        }
    }

    if (receiver && !selectedUrls.isEmpty()) {
        QMetaObject::invokeMethod(receiver.data(), "catch_urls_open", Qt::QueuedConnection,
                                  Q_ARG(QStringList, selectedUrls));
    }
    if (receiver && errorMessages.count > 0) {
        const QString summary = fromNSString([errorMessages componentsJoinedByString:@"\n"]);
        QMetaObject::invokeMethod(receiver.data(), "catch_picker_error", Qt::QueuedConnection,
                                  Q_ARG(QString, summary));
    }
    pickerActive = false;
}

@end


@interface GyroflowVideoPickerDelegate : NSObject <PHPickerViewControllerDelegate> {
    QPointer<QObject> receiver;
}
- (instancetype)initWithReceiver:(QObject *)receiver;
@end

@implementation GyroflowVideoPickerDelegate

- (instancetype)initWithReceiver:(QObject *)receiverObject
{
    self = [super init];
    if (self)
        receiver = receiverObject;
    return self;
}

- (void)picker:(PHPickerViewController *)picker didFinishPicking:(NSArray<PHPickerResult *> *)results
{
    [picker dismissViewControllerAnimated:YES completion:nil];
    if (results.count == 0) {
        pickerActive = false;
        invokeCancelled(receiver);
        return;
    }

    const QString sessionPath = QDir(importRootPath()).filePath(
        fromNSString(NSUUID.UUID.UUIDString));
    if (!QDir().mkpath(sessionPath)) {
        pickerActive = false;
        if (receiver) {
            QMetaObject::invokeMethod(receiver.data(), "catch_picker_error", Qt::QueuedConnection,
                                      Q_ARG(QString, QStringLiteral("Unable to create the video import cache.")));
        }
        return;
    }

    GyroflowVideoImportSession *session = [[GyroflowVideoImportSession alloc]
        initWithReceiver:receiver.data()
        count:results.count
        directory:toNSString(sessionPath)];

    [results enumerateObjectsUsingBlock:^(PHPickerResult *result, NSUInteger index, BOOL *) {
        NSItemProvider *provider = result.itemProvider;
        NSString *typeIdentifier = videoTypeIdentifier(provider);
        NSString *label = provider.suggestedName.length
            ? provider.suggestedName
            : [NSString stringWithFormat:@"Video %lu", static_cast<unsigned long>(index + 1)];
        if (!typeIdentifier) {
            [session finishItemAtIndex:index
                                  path:nil
                                 error:[NSString stringWithFormat:@"%@: Unsupported video type.", label]];
            return;
        }

        [provider loadFileRepresentationForTypeIdentifier:typeIdentifier
                                        completionHandler:^(NSURL *url, NSError *loadError) {
            if (!url || loadError) {
                NSString *detail = loadError.localizedDescription ?: @"The video could not be read.";
                [session finishItemAtIndex:index
                                      path:nil
                                     error:[NSString stringWithFormat:@"%@: %@", label, detail]];
                return;
            }

            NSString *itemDirectory = [session->directory
                stringByAppendingPathComponent:NSUUID.UUID.UUIDString];
            NSError *directoryError = nil;
            if (![NSFileManager.defaultManager createDirectoryAtPath:itemDirectory
                                          withIntermediateDirectories:YES
                                                           attributes:nil
                                                                error:&directoryError]) {
                [session finishItemAtIndex:index
                                      path:nil
                                     error:[NSString stringWithFormat:@"%@: %@", label,
                                            directoryError.localizedDescription]];
                return;
            }

            NSString *filename = destinationFilename(provider, url, typeIdentifier);
            NSURL *destination = [NSURL fileURLWithPath:
                [itemDirectory stringByAppendingPathComponent:filename]];
            NSError *copyError = nil;
            if (![NSFileManager.defaultManager copyItemAtURL:url toURL:destination error:&copyError]) {
                [session finishItemAtIndex:index
                                      path:nil
                                     error:[NSString stringWithFormat:@"%@: %@", label,
                                            copyError.localizedDescription]];
                return;
            }
            [session finishItemAtIndex:index path:destination.path error:nil];
        }];
    }];
    [session release];
}

@end


bool gyroflowIosOpenVideoPicker(QObject *receiver)
{
    if (!receiver || pickerActive || !NSThread.isMainThread)
        return false;

    UIWindow *window = activeWindow();
    UIViewController *controller = topViewController(window.rootViewController);
    if (!controller)
        return false;

    PHPickerConfiguration *configuration =
        [[[PHPickerConfiguration alloc] initWithPhotoLibrary:PHPhotoLibrary.sharedPhotoLibrary]
            autorelease];
    configuration.filter = PHPickerFilter.videosFilter;
    configuration.selectionLimit = 0;
    configuration.preferredAssetRepresentationMode =
        PHPickerConfigurationAssetRepresentationModeCurrent;

    PHPickerViewController *picker =
        [[[PHPickerViewController alloc] initWithConfiguration:configuration] autorelease];
    GyroflowVideoPickerDelegate *delegate =
        [[GyroflowVideoPickerDelegate alloc] initWithReceiver:receiver];
    picker.delegate = delegate;
    objc_setAssociatedObject(picker, &delegateAssociationKey, delegate,
                             OBJC_ASSOCIATION_RETAIN_NONATOMIC);
    [delegate release];

    pickerActive = true;
    [controller presentViewController:picker animated:YES completion:nil];
    return true;
}

void gyroflowIosCleanupVideoImports()
{
    const QString rootPath = importRootPath();
    QDir root(rootPath);
    if (root.exists())
        root.removeRecursively();
    QDir().mkpath(rootPath);
}
