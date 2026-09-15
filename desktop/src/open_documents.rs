//! The files macOS hands squint: a double-click in Finder, a file dropped on
//! the Dock icon, Open With, `open -a squint`.
//!
//! None of these is on the command line. They arrive as an Open Documents
//! Apple Event, to a squint just started for them or to one already running,
//! and winit has no door for it. AppKit installs its own handler for the event
//! as it finishes launching and dispatches a launch's files before the
//! application has finished, so squint's handler goes in when launching is
//! about to finish: after AppKit's, which it replaces, and before the files.
//! They join the queue the window takes from (see `handoff`).

use crate::handoff;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{AllocAnyThread, define_class, msg_send, sel};
use objc2_foundation::{
    NSAppleEventDescriptor, NSAppleEventManager, NSNotification, NSNotificationCenter,
};
use std::path::PathBuf;

/// `'aevt'`, the class of the core Apple Events.
const CORE_EVENT_CLASS: u32 = u32::from_be_bytes(*b"aevt");
/// `'odoc'`, Open Documents.
const OPEN_DOCUMENTS: u32 = u32::from_be_bytes(*b"odoc");
/// `'----'`, the event's direct object: here, the files.
const DIRECT_OBJECT: u32 = u32::from_be_bytes(*b"----");

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "SquintOpenDocuments"]
    struct Opener;

    impl Opener {
        #[unsafe(method(willFinishLaunching:))]
        fn will_finish_launching(&self, _note: &NSNotification) {
            let manager = NSAppleEventManager::sharedAppleEventManager();
            // SAFETY: `self` answers the selector with the arguments the
            // manager passes, and lives for the rest of the process.
            let () = unsafe {
                msg_send![
                    &*manager,
                    setEventHandler: self,
                    andSelector: sel!(openDocuments:withReplyEvent:),
                    forEventClass: CORE_EVENT_CLASS,
                    andEventID: OPEN_DOCUMENTS
                ]
            };
        }

        #[unsafe(method(openDocuments:withReplyEvent:))]
        fn open_documents(&self, event: &NSAppleEventDescriptor, _reply: &NSAppleEventDescriptor) {
            handoff::push(files_in(event));
            crate::native_menu::wake();
        }
    }
);

/// Starts listening for Open Documents. Called once, before the event loop
/// runs.
pub fn listen() {
    // SAFETY: a plain NSObject subclass with no instance variables.
    let opener: Retained<Opener> = unsafe { msg_send![Opener::alloc(), init] };
    let center = NSNotificationCenter::defaultCenter();
    // SAFETY: the observer answers the selector, which takes the notification,
    // and is never released, so the centre never holds a dangling one.
    unsafe {
        center.addObserver_selector_name_object(
            &opener,
            sel!(willFinishLaunching:),
            Some(objc2_app_kit::NSApplicationWillFinishLaunchingNotification),
            None::<&AnyObject>,
        );
    }
    std::mem::forget(opener);
}

/// The files an Open Documents event names: a list of them, or one.
fn files_in(event: &NSAppleEventDescriptor) -> Vec<PathBuf> {
    // SAFETY: the keyword is a four-character code, as the method takes.
    let direct: Option<Retained<NSAppleEventDescriptor>> =
        unsafe { msg_send![event, paramDescriptorForKeyword: DIRECT_OBJECT] };
    let Some(direct) = direct else {
        return Vec::new();
    };
    let count = direct.numberOfItems();
    let items: Vec<Retained<NSAppleEventDescriptor>> = if count > 0 {
        (1..=count)
            .filter_map(|i| direct.descriptorAtIndex(i))
            .collect()
    } else {
        vec![direct]
    };
    items
        .iter()
        .filter_map(|item| item.fileURLValue())
        .filter_map(|url| url.path())
        .map(|path| PathBuf::from(path.to_string()))
        .collect()
}
