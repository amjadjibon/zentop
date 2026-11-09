//! UserNotifications delivery without changing the process's bundle identity.
use block2::RcBlock;
use objc2::{rc::autoreleasepool, runtime::Bool};
use objc2_foundation::{NSBundle, NSError, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotificationRequest,
    UNUserNotificationCenter,
};
use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);
fn callback_error(error: *mut NSError) -> Option<String> {
    // SAFETY: Apple supplies a nullable NSError valid during this callback.
    unsafe { error.as_ref() }.map(|e| e.localizedDescription().to_string())
}
fn wait(receiver: Receiver<Result<(), String>>, stage: &str) -> Result<(), String> {
    receiver
        .recv_timeout(Duration::from_secs(15))
        .map_err(|e| format!("macOS notification {stage} timed out or stopped: {e}"))?
}
/// Runs on the bounded notification worker, including all completion waits.
pub(super) fn send(message: &str) -> Result<(), String> {
    autoreleasepool(|_| {
        objc2::exception::catch(|| deliver(message))
            .map_err(|e| format!("macOS notification service failed: {e:?}"))?
    })
}
fn deliver(message: &str) -> Result<(), String> {
    let bundle = NSBundle::mainBundle();
    if bundle.bundleIdentifier().is_none() || !bundle.bundlePath().to_string().ends_with(".app") {
        return Err("Desktop notifications on macOS require launching the Zentop app bundle. Alerts remain available in history.".into());
    }
    // Check the bundle before this call: the native API can throw for an
    // unbundled executable. Native exceptions are caught by the outer wrapper.
    let center = UNUserNotificationCenter::currentNotificationCenter();
    let (tx, rx) = mpsc::sync_channel(1);
    let authorization = RcBlock::new(move |granted: Bool, error: *mut NSError| {
        let result = match callback_error(error) {
            Some(error) => Err(error),
            None if granted.as_bool() => Ok(()),
            None => Err("Desktop notifications are disabled for Zentop in System Settings.".into()),
        };
        let _ = tx.try_send(result);
    });
    center.requestAuthorizationWithOptions_completionHandler(
        UNAuthorizationOptions::Alert,
        &authorization,
    );
    wait(rx, "authorization")?;
    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str("Zentop resource alert"));
    content.setBody(&NSString::from_str(message));
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let identifier = NSString::from_str(&format!(
        "zentop-{}-{time}-{}",
        std::process::id(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let request =
        UNNotificationRequest::requestWithIdentifier_content_trigger(&identifier, &content, None);
    let (tx, rx) = mpsc::sync_channel(1);
    let completion = RcBlock::new(move |error: *mut NSError| {
        let result = callback_error(error).map_or(Ok(()), Err);
        let _ = tx.try_send(result);
    });
    center.addNotificationRequest_withCompletionHandler(&request, Some(&completion));
    wait(rx, "submission")
}
