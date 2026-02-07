// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! AppKit window and controls.
//!
//! Creates an NSWindow with labels for rig status and buttons for rig
//! control.  All AppKit calls must run on the main thread.

use objc2::rc::Retained;
use objc2::{msg_send, MainThreadMarker};
use objc2_app_kit::{
    NSBackingStoreType, NSButton, NSColor, NSStackView, NSTextField, NSView, NSWindow,
    NSWindowStyleMask,
};
use objc2_foundation::{NSArray, NSEdgeInsets, NSPoint, NSRect, NSSize, NSString};

use tokio::sync::{mpsc, oneshot};
use tracing::warn;

use trx_core::radio::freq::Freq;
use trx_core::rig::command::RigCommand;
use trx_core::rig::request::RigRequest;

use crate::helpers::parse_mode;
use crate::model::RigStateModel;

/// All UI elements that need updating when rig state changes.
/// These must only be accessed from the AppKit main thread.
pub struct UiElements {
    pub freq_label: Retained<NSTextField>,
    pub mode_label: Retained<NSTextField>,
    pub band_label: Retained<NSTextField>,
    pub ptt_label: Retained<NSTextField>,
    pub lock_label: Retained<NSTextField>,
    pub power_label: Retained<NSTextField>,
    pub rx_sig_label: Retained<NSTextField>,
    pub tx_power_label: Retained<NSTextField>,
    pub tx_limit_label: Retained<NSTextField>,
    pub tx_swr_label: Retained<NSTextField>,
    pub tx_alc_label: Retained<NSTextField>,
    pub vfo_label: Retained<NSTextField>,
    // Input fields for reading user input from button actions
    pub freq_input: Retained<NSTextField>,
    pub mode_input: Retained<NSTextField>,
    pub tx_limit_input: Retained<NSTextField>,
}

impl UiElements {
    /// Refresh all labels from the model.
    pub fn refresh(&self, model: &RigStateModel) {
        set_label_text(&self.freq_label, &model.freq_text);
        set_label_text(&self.mode_label, &format!("Mode: {}", model.mode));
        set_label_text(&self.band_label, &format!("Band: {}", model.band));
        set_label_text(
            &self.ptt_label,
            if model.tx_enabled { "PTT: TX" } else { "PTT: RX" },
        );
        set_label_text(
            &self.lock_label,
            if model.locked {
                "Lock: ON"
            } else {
                "Lock: OFF"
            },
        );
        set_label_text(
            &self.power_label,
            if model.powered {
                "Power: ON"
            } else {
                "Power: OFF"
            },
        );
        set_label_text(&self.rx_sig_label, &format!("RX Sig: {}", model.rx_sig));
        set_label_text(
            &self.tx_power_label,
            &format!("TX Power: {}", model.tx_power),
        );
        set_label_text(
            &self.tx_limit_label,
            &format!("TX Limit: {}", model.tx_limit),
        );
        set_label_text(
            &self.tx_swr_label,
            &format!("SWR: {:.1}", model.tx_swr),
        );
        set_label_text(&self.tx_alc_label, &format!("ALC: {}", model.tx_alc));
        set_label_text(&self.vfo_label, &format!("VFO: {}", model.vfo));
    }

    /// Read the frequency input field value and send a SetFreq command.
    pub fn handle_set_freq(&self, rig_tx: &mpsc::Sender<RigRequest>) {
        let val = self.freq_input.stringValue();
        let text = val.to_string();
        if let Ok(hz) = text.trim().parse::<u64>() {
            if hz > 0 {
                send_command(rig_tx, RigCommand::SetFreq(Freq { hz }));
            }
        }
    }

    /// Read the mode input field value and send a SetMode command.
    pub fn handle_set_mode(&self, rig_tx: &mpsc::Sender<RigRequest>) {
        let val = self.mode_input.stringValue();
        let mode = parse_mode(&val.to_string());
        send_command(rig_tx, RigCommand::SetMode(mode));
    }

    /// Read the TX limit input field value and send a SetTxLimit command.
    pub fn handle_set_tx_limit(&self, rig_tx: &mpsc::Sender<RigRequest>) {
        let val = self.tx_limit_input.stringValue();
        if let Ok(limit) = val.to_string().trim().parse::<u8>() {
            send_command(rig_tx, RigCommand::SetTxLimit(limit));
        }
    }
}

fn set_label_text(label: &NSTextField, text: &str) {
    let ns = NSString::from_str(text);
    label.setStringValue(&ns);
}

fn make_label(mtm: MainThreadMarker, text: &str) -> Retained<NSTextField> {
    let ns = NSString::from_str(text);
    let label = NSTextField::labelWithString(&ns, mtm);
    label.setEditable(false);
    label.setBordered(false);
    label.setDrawsBackground(false);
    label.setTextColor(Some(&NSColor::labelColor()));
    label
}

fn make_editable_field(mtm: MainThreadMarker, placeholder: &str) -> Retained<NSTextField> {
    let ns = NSString::from_str(placeholder);
    let field = NSTextField::textFieldWithString(&ns, mtm);
    field.setEditable(true);
    field.setBordered(true);
    field
}

/// Convert an NSTextField into an NSView (NSTextField -> NSControl -> NSView).
fn text_field_to_view(field: Retained<NSTextField>) -> Retained<NSView> {
    Retained::into_super(Retained::into_super(field))
}

/// Convert an NSButton into an NSView (NSButton -> NSControl -> NSView).
fn button_to_view(btn: Retained<NSButton>) -> Retained<NSView> {
    Retained::into_super(Retained::into_super(btn))
}

/// Actions that buttons can trigger. Sent via a channel to be handled
/// on the AppKit thread where UI elements live.
#[derive(Debug, Clone, Copy)]
pub enum ButtonAction {
    TogglePtt,
    TogglePower,
    ToggleVfo,
    ToggleLock,
    SetFreq,
    SetMode,
    SetTxLimit,
}

/// Build the main window with status labels and control buttons.
///
/// `action_tx` is a channel sender for button actions — each button stores
/// its action tag and the server's run-loop timer reads these to dispatch.
///
/// Returns the window (which must be kept alive) and the UI elements
/// struct for later updates.
pub fn build_window(
    mtm: MainThreadMarker,
    action_tx: std::sync::mpsc::Sender<ButtonAction>,
) -> (Retained<NSWindow>, UiElements) {
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Miniaturizable
        | NSWindowStyleMask::Resizable;

    let frame = NSRect::new(NSPoint::new(200.0, 200.0), NSSize::new(400.0, 520.0));
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            mtm.alloc::<NSWindow>(),
            frame,
            style,
            NSBackingStoreType::Buffered,
            false,
        )
    };

    let title = NSString::from_str("trx-rs");
    window.setTitle(&title);

    // Status labels
    let freq_label = make_label(mtm, "-- Hz");
    let mode_label = make_label(mtm, "Mode: --");
    let band_label = make_label(mtm, "Band: --");
    let ptt_label = make_label(mtm, "PTT: RX");
    let lock_label = make_label(mtm, "Lock: OFF");
    let power_label = make_label(mtm, "Power: OFF");
    let rx_sig_label = make_label(mtm, "RX Sig: 0");
    let tx_power_label = make_label(mtm, "TX Power: 0");
    let tx_limit_label = make_label(mtm, "TX Limit: 0");
    let tx_swr_label = make_label(mtm, "SWR: 0.0");
    let tx_alc_label = make_label(mtm, "ALC: 0");
    let vfo_label = make_label(mtm, "VFO: --");

    // Control buttons — each stores an action tag, actions are dispatched
    // via the global action table.
    let ptt_btn = make_button(mtm, "Toggle PTT", ButtonAction::TogglePtt, &action_tx);
    let power_btn = make_button(mtm, "Toggle Power", ButtonAction::TogglePower, &action_tx);
    let vfo_btn = make_button(mtm, "Toggle VFO", ButtonAction::ToggleVfo, &action_tx);
    let lock_btn = make_button(mtm, "Toggle Lock", ButtonAction::ToggleLock, &action_tx);

    // Input fields
    let freq_input = make_editable_field(mtm, "Freq (Hz)");
    let set_freq_btn = make_button(mtm, "Set Freq", ButtonAction::SetFreq, &action_tx);

    let mode_input = make_editable_field(mtm, "Mode (USB, LSB, ...)");
    let set_mode_btn = make_button(mtm, "Set Mode", ButtonAction::SetMode, &action_tx);

    let tx_limit_input = make_editable_field(mtm, "TX Limit (0-255)");
    let set_tx_limit_btn = make_button(mtm, "Set TX Limit", ButtonAction::SetTxLimit, &action_tx);

    // Build vertical stack view
    let views: Vec<Retained<NSView>> = vec![
        text_field_to_view(freq_label.clone()),
        text_field_to_view(mode_label.clone()),
        text_field_to_view(band_label.clone()),
        text_field_to_view(ptt_label.clone()),
        text_field_to_view(lock_label.clone()),
        text_field_to_view(power_label.clone()),
        text_field_to_view(rx_sig_label.clone()),
        text_field_to_view(tx_power_label.clone()),
        text_field_to_view(tx_limit_label.clone()),
        text_field_to_view(tx_swr_label.clone()),
        text_field_to_view(tx_alc_label.clone()),
        text_field_to_view(vfo_label.clone()),
        button_to_view(ptt_btn),
        button_to_view(power_btn),
        button_to_view(vfo_btn),
        button_to_view(lock_btn),
        text_field_to_view(freq_input.clone()),
        button_to_view(set_freq_btn),
        text_field_to_view(mode_input.clone()),
        button_to_view(set_mode_btn),
        text_field_to_view(tx_limit_input.clone()),
        button_to_view(set_tx_limit_btn),
    ];

    let ns_views = NSArray::from_retained_slice(&views);
    let stack = NSStackView::stackViewWithViews(&ns_views, mtm);
    stack.setOrientation(objc2_app_kit::NSUserInterfaceLayoutOrientation::Vertical);
    stack.setSpacing(8.0);
    stack.setEdgeInsets(NSEdgeInsets {
        top: 16.0,
        left: 16.0,
        bottom: 16.0,
        right: 16.0,
    });

    window.setContentView(Some(&stack));
    window.makeKeyAndOrderFront(None);

    let ui = UiElements {
        freq_label,
        mode_label,
        band_label,
        ptt_label,
        lock_label,
        power_label,
        rx_sig_label,
        tx_power_label,
        tx_limit_label,
        tx_swr_label,
        tx_alc_label,
        vfo_label,
        freq_input,
        mode_input,
        tx_limit_input,
    };

    (window, ui)
}

fn send_command(tx: &mpsc::Sender<RigRequest>, cmd: RigCommand) {
    let (resp_tx, _resp_rx) = oneshot::channel();
    if tx
        .blocking_send(RigRequest {
            cmd,
            respond_to: resp_tx,
        })
        .is_err()
    {
        warn!("AppKit frontend: rig command send failed");
    }
}

fn make_button(
    mtm: MainThreadMarker,
    title: &str,
    action: ButtonAction,
    action_tx: &std::sync::mpsc::Sender<ButtonAction>,
) -> Retained<NSButton> {
    let title_ns = NSString::from_str(title);
    let btn = unsafe {
        NSButton::buttonWithTitle_target_action(&title_ns, None, None, mtm)
    };

    // Store the action in the global table, indexed by the button's tag.
    let idx = register_button_action(action, action_tx.clone());
    unsafe {
        let _: () = msg_send![&btn, setTag: idx as isize];
    };

    btn
}

// Global action table for buttons.
struct ActionEntry {
    action: ButtonAction,
    sender: std::sync::mpsc::Sender<ButtonAction>,
}

static BUTTON_ACTIONS: std::sync::OnceLock<std::sync::Mutex<Vec<ActionEntry>>> =
    std::sync::OnceLock::new();

fn action_table() -> &'static std::sync::Mutex<Vec<ActionEntry>> {
    BUTTON_ACTIONS.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

fn register_button_action(
    action: ButtonAction,
    sender: std::sync::mpsc::Sender<ButtonAction>,
) -> usize {
    let mut table = action_table().lock().unwrap();
    let idx = table.len();
    table.push(ActionEntry { action, sender });
    idx
}

/// Call the action for a button given its tag index.
/// This sends the button's action through the channel for processing
/// on the main thread where UI elements are accessible.
pub fn invoke_button_action(tag: isize) {
    let table = action_table().lock().unwrap();
    if let Some(entry) = table.get(tag as usize) {
        let _ = entry.sender.send(entry.action);
    }
}
