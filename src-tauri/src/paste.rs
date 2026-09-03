//! Livraison du texte : détection d'un champ texte actif, collage simulé,
//! presse-papiers. Les fonctions `paste_*` s'exécutent sur le thread principal
//! (contrainte des API macOS utilisées par enigo).

use anyhow::Result;

/// Vrai si l'application au premier plan a un champ texte éditable focalisé.
/// Nécessite l'autorisation Accessibilité sur macOS ; sinon renvoie `false`.
pub fn focused_text_field() -> bool {
    imp::focused_text_field()
}

/// Copie `text` dans le presse-papiers (mode « sans champ actif »).
pub fn copy(text: &str) -> Result<()> {
    let mut cb = arboard::Clipboard::new()?;
    cb.set_text(text.to_string())?;
    Ok(())
}

/// Insère `text` dans le champ actif via le presse-papiers + Cmd/Ctrl+V, puis
/// restaure le contenu texte précédent du presse-papiers.
pub fn paste_into_focused(text: &str) -> Result<()> {
    let mut cb = arboard::Clipboard::new()?;
    let previous = cb.get_text().ok();
    cb.set_text(text.to_string())?;
    std::thread::sleep(std::time::Duration::from_millis(40));
    let sent = send_paste();
    if let Some(prev) = previous {
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(350));
            if let Ok(mut cb) = arboard::Clipboard::new() {
                let _ = cb.set_text(prev);
            }
        });
    }
    sent
}

/// Tape `text` dans le champ actif (événements clavier unicode, sans passer
/// par le presse-papiers). Adapté aux textes courts (provisoires).
pub fn type_text(text: &str) -> Result<()> {
    use enigo::{Enigo, Keyboard, Settings};
    if text.is_empty() {
        return Ok(());
    }
    let mut enigo = Enigo::new(&Settings::default()).map_err(|e| anyhow::anyhow!("{e:?}"))?;
    enigo.text(text).map_err(|e| anyhow::anyhow!("{e:?}"))
}

/// Efface `n` caractères (graphèmes) avant le curseur.
pub fn backspace(n: usize) -> Result<()> {
    use enigo::{Direction, Enigo, Key, Keyboard, Settings};
    if n == 0 {
        return Ok(());
    }
    let mut enigo = Enigo::new(&Settings::default()).map_err(|e| anyhow::anyhow!("{e:?}"))?;
    for _ in 0..n {
        enigo.key(Key::Backspace, Direction::Click).map_err(|e| anyhow::anyhow!("{e:?}"))?;
    }
    Ok(())
}

pub fn grapheme_len(s: &str) -> usize {
    use unicode_segmentation::UnicodeSegmentation;
    s.graphemes(true).count()
}

fn send_paste() -> Result<()> {
    use enigo::{Direction, Enigo, Key, Keyboard, Settings};
    let mut enigo = Enigo::new(&Settings::default()).map_err(|e| anyhow::anyhow!("{e:?}"))?;
    #[cfg(target_os = "macos")]
    let modifier = Key::Meta;
    #[cfg(not(target_os = "macos"))]
    let modifier = Key::Control;
    // Touche physique V (kVK_ANSI_V = 9) : évite la résolution de disposition
    // clavier d'enigo, qui exige le thread principal sur macOS.
    #[cfg(target_os = "macos")]
    let v_key = Key::Other(9);
    #[cfg(not(target_os = "macos"))]
    let v_key = Key::Unicode('v');
    enigo.key(modifier, Direction::Press).map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let r = enigo.key(v_key, Direction::Click);
    enigo.key(modifier, Direction::Release).map_err(|e| anyhow::anyhow!("{e:?}"))?;
    r.map_err(|e| anyhow::anyhow!("{e:?}"))?;
    Ok(())
}

#[cfg(target_os = "macos")]
mod imp {
    use accessibility_sys::{
        kAXErrorSuccess, AXIsProcessTrusted, AXUIElementCopyAttributeValue, AXUIElementCreateSystemWide,
        AXUIElementIsAttributeSettable, AXUIElementRef,
    };
    use core_foundation::base::TCFType;
    use core_foundation::string::CFString;
    use core_foundation_sys::base::{Boolean, CFRelease, CFTypeRef};

    unsafe fn attr(elem: AXUIElementRef, name: &str) -> Option<CFTypeRef> {
        let key = CFString::new(name);
        let mut out: CFTypeRef = std::ptr::null();
        let err = AXUIElementCopyAttributeValue(elem, key.as_concrete_TypeRef(), &mut out);
        if err == kAXErrorSuccess && !out.is_null() {
            Some(out)
        } else {
            None
        }
    }

    unsafe fn settable(elem: AXUIElementRef, name: &str) -> bool {
        let key = CFString::new(name);
        let mut s: Boolean = 0;
        AXUIElementIsAttributeSettable(elem, key.as_concrete_TypeRef(), &mut s) == kAXErrorSuccess && s != 0
    }

    pub fn focused_text_field() -> bool {
        unsafe {
            if !AXIsProcessTrusted() {
                return false;
            }
            let system = AXUIElementCreateSystemWide();
            let focused = attr(system, "AXFocusedUIElement");
            CFRelease(system as CFTypeRef);
            let Some(focused) = focused else { return false };
            let elem = focused as AXUIElementRef;

            let role = attr(elem, "AXRole")
                .map(|r| {
                    let s = CFString::wrap_under_create_rule(r as _).to_string();
                    s
                })
                .unwrap_or_default();
            let has_selection = match attr(elem, "AXSelectedTextRange") {
                Some(v) => {
                    CFRelease(v);
                    true
                }
                None => false,
            };
            let value_settable = settable(elem, "AXValue");
            CFRelease(elem as CFTypeRef);

            let text_role = matches!(role.as_str(), "AXTextField" | "AXTextArea" | "AXComboBox" | "AXSearchField");
            let result = text_role || (has_selection && value_settable) || (role == "AXWebArea" && has_selection);
            log::debug!("focus AX : role={role} sel={has_selection} settable={value_settable} → {result}");
            result
        }
    }
}

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetGUIThreadInfo, GetWindowThreadProcessId, GUITHREADINFO};

    pub fn focused_text_field() -> bool {
        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd.is_null() {
                return false;
            }
            let tid = GetWindowThreadProcessId(hwnd, std::ptr::null_mut());
            let mut info: GUITHREADINFO = std::mem::zeroed();
            info.cbSize = std::mem::size_of::<GUITHREADINFO>() as u32;
            if GetGUIThreadInfo(tid, &mut info) == 0 {
                return false;
            }
            // Un caret visible signale un champ texte actif.
            !info.hwndCaret.is_null()
        }
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod imp {
    pub fn focused_text_field() -> bool {
        false
    }
}
