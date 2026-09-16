#import <AppKit/AppKit.h>
#import <Carbon/Carbon.h>

// Handle only Finder/Dock reopen AppleEvents. Ordinary activation (including
// a menu-bar click or a policy change) must not undo a user's hide action.
// Keep Qt's NSApplication delegate: it still owns quit, menus and file opens.
@interface QbzReopenHandler : NSObject
@property(nonatomic, assign) void (*presentWindow)(void);
- (void)reopen:(NSAppleEventDescriptor *)event reply:(NSAppleEventDescriptor *)reply;
- (void)installAfterLaunch:(NSNotification *)notification;
@end

@implementation QbzReopenHandler
- (void)installAfterLaunch:(NSNotification *)notification
{
    (void)notification;
    // AppKit installs its defaults during launch. Register after that so the
    // first launch cannot silently replace our reopen handler.
    [[NSAppleEventManager sharedAppleEventManager]
        setEventHandler:self andSelector:@selector(reopen:reply:)
        forEventClass:kCoreEventClass andEventID:kAEReopenApplication];
    [[NSNotificationCenter defaultCenter] removeObserver:self
        name:NSApplicationDidFinishLaunchingNotification object:nil];
}

- (void)reopen:(NSAppleEventDescriptor *)event reply:(NSAppleEventDescriptor *)reply
{
    (void)event;
    (void)reply;
    if (self.presentWindow)
        self.presentWindow();
}
@end

extern "C" void qbz_install_macos_reopen_handler(void (*presentWindow)(void))
{
    // Main thread, one owner for the entire application lifetime. QML can
    // boot before exec(), when AppKit has not finished launching yet.
    static QbzReopenHandler *handler = nil;
    if (handler)
        return;
    handler = [[QbzReopenHandler alloc] init];
    handler.presentWindow = presentWindow;
    if (NSApp.isRunning) {
        [handler installAfterLaunch:nil];
    } else {
        [[NSNotificationCenter defaultCenter] addObserver:handler
            selector:@selector(installAfterLaunch:)
            name:NSApplicationDidFinishLaunchingNotification object:nil];
    }
}
