//! Engine options dialog: the UCI options an engine declared, each with a
//! control of the right kind. A change is reported at once as an
//! [`OptionsAction`], and the app sends it to the engine. The dialog holds
//! only the values it shows.

use std::collections::BTreeMap;

use eframe::egui::{self, ComboBox, DragValue, TextEdit, Ui};

use crate::engine::protocol::{UciOption, UciOptionKind};

/// What the dialog asked for this frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OptionsAction {
    /// Nothing changed.
    None,
    /// Set an option to a new value (`check` as `true`/`false`).
    Set {
        /// Option name, as the engine declared it.
        name: String,
        /// The new value in UCI text form.
        value: String,
    },
    /// Press a button option.
    Press {
        /// Option name, as the engine declared it.
        name: String,
    },
    /// Set an option back to the engine's default, and forget the saved value.
    Reset {
        /// Option name, as the engine declared it.
        name: String,
        /// The engine's default, in UCI text form.
        value: String,
    },
    /// Close the dialog.
    Close,
}

/// The engine's default for an option, in UCI text form. A button has none.
pub fn default_value(option: &UciOption) -> String {
    match &option.kind {
        UciOptionKind::Check { default } => default.to_string(),
        UciOptionKind::Spin { default, .. } => default.to_string(),
        UciOptionKind::Combo { default, .. } => default.clone(),
        UciOptionKind::String { default } => default.clone(),
        UciOptionKind::Button => String::new(),
    }
}

/// The value an option shows: the saved value when there is one, else the
/// engine's default. A button has no value.
pub fn value_for(option: &UciOption, settings: &BTreeMap<String, String>) -> String {
    match settings.get(&option.name) {
        Some(saved) => saved.clone(),
        None => default_value(option),
    }
}

/// The options window for one engine slot.
pub struct OptionsDialog {
    /// Slot the engine is in (`0` = A, `1` = B).
    slot: usize,
    /// Window title, e.g. `Engine A options`.
    title: String,
    /// The options the engine declared.
    options: Vec<UciOption>,
    /// The values shown, by option name. Buttons have no entry.
    values: BTreeMap<String, String>,
}

impl OptionsDialog {
    /// A dialog for the options of the engine in `slot`, showing `settings`
    /// where the user has set a value.
    pub fn new(slot: usize, letter: char, options: Vec<UciOption>, settings: &BTreeMap<String, String>) -> Self {
        let values = options
            .iter()
            .filter(|option| !matches!(option.kind, UciOptionKind::Button))
            .map(|option| (option.name.clone(), value_for(option, settings)))
            .collect();
        Self {
            slot,
            title: format!("Engine {} options", letter),
            options,
            values,
        }
    }

    /// The slot this dialog is for.
    pub fn slot(&self) -> usize {
        self.slot
    }

    /// Draw the window and report what the user did.
    pub fn show(&mut self, ctx: &egui::Context) -> OptionsAction {
        let mut action = OptionsAction::None;
        let options = &self.options;
        let values = &mut self.values;
        egui::Window::new(self.title.as_str())
            .collapsible(false)
            .resizable(true)
            .default_width(440.0)
            .max_height(520.0)
            .vscroll(true)
            .show(ctx, |ui| {
                if options.is_empty() {
                    ui.label("This engine declares no options.");
                }
                for option in options {
                    let change = show_option(ui, option, values);
                    if change != OptionsAction::None {
                        action = change;
                    }
                }
                ui.separator();
                if ui.button("Close").clicked() {
                    action = OptionsAction::Close;
                }
            });
        action
    }
}

/// Draw one option with the control for its kind and return its change.
/// Every option except a button gets a Default button after its control.
fn show_option(ui: &mut Ui, option: &UciOption, values: &mut BTreeMap<String, String>) -> OptionsAction {
    let change = show_control(ui, option, values);
    if matches!(option.kind, UciOptionKind::Button) {
        return change;
    }
    if ui.small_button("Default").clicked() {
        let value = default_value(option);
        values.insert(option.name.clone(), value.clone());
        return OptionsAction::Reset {
            name: option.name.clone(),
            value,
        };
    }
    change
}

/// The control for an option's kind, and the change it made.
fn show_control(ui: &mut Ui, option: &UciOption, values: &mut BTreeMap<String, String>) -> OptionsAction {
    let name = option.name.clone();
    match &option.kind {
        UciOptionKind::Check { .. } => {
            let mut on = values.get(&name).is_some_and(|value| value == "true");
            if ui.checkbox(&mut on, &name).changed() {
                let value = on.to_string();
                values.insert(name.clone(), value.clone());
                return OptionsAction::Set { name, value };
            }
            OptionsAction::None
        }
        UciOptionKind::Spin { min, max, default } => {
            let mut number = values
                .get(&name)
                .and_then(|value| value.parse::<i64>().ok())
                .unwrap_or(*default);
            let changed = ui
                .horizontal(|ui| {
                    ui.label(&name);
                    ui.add(DragValue::new(&mut number).range(*min..=*max)).changed()
                })
                .inner;
            if changed {
                let value = number.to_string();
                values.insert(name.clone(), value.clone());
                return OptionsAction::Set { name, value };
            }
            OptionsAction::None
        }
        UciOptionKind::Combo { values: choices, .. } => {
            let current = values.get(&name).cloned().unwrap_or_default();
            let mut chosen: Option<String> = None;
            ui.horizontal(|ui| {
                ui.label(&name);
                ComboBox::from_id_salt(&name)
                    .selected_text(current.as_str())
                    .show_ui(ui, |ui| {
                        for choice in choices {
                            if ui.selectable_label(*choice == current, choice).clicked() {
                                chosen = Some(choice.clone());
                            }
                        }
                    });
            });
            match chosen {
                Some(value) => {
                    values.insert(name.clone(), value.clone());
                    OptionsAction::Set { name, value }
                }
                None => OptionsAction::None,
            }
        }
        UciOptionKind::Button => {
            if ui.button(&name).clicked() {
                OptionsAction::Press { name }
            } else {
                OptionsAction::None
            }
        }
        UciOptionKind::String { .. } => {
            let text = values.entry(name.clone()).or_default();
            let response = ui
                .horizontal(|ui| {
                    ui.label(&name);
                    ui.add(TextEdit::singleline(text))
                })
                .inner;
            // A text value is sent when the field is left (or Enter is pressed), not on every key.
            if response.lost_focus() {
                let value = values.get(&name).cloned().unwrap_or_default();
                return OptionsAction::Set { name, value };
            }
            OptionsAction::None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn option(name: &str, kind: UciOptionKind) -> UciOption {
        UciOption {
            name: name.to_string(),
            kind,
        }
    }

    #[test]
    fn a_saved_value_overrides_the_default() {
        let hash = option(
            "Hash",
            UciOptionKind::Spin {
                default: 16,
                min: 1,
                max: 1024,
            },
        );
        let mut settings = BTreeMap::new();
        assert_eq!(value_for(&hash, &settings), "16");
        settings.insert("Hash".to_string(), "256".to_string());
        assert_eq!(value_for(&hash, &settings), "256");
    }

    #[test]
    fn default_value_ignores_the_saved_one() {
        let hash = option(
            "Hash",
            UciOptionKind::Spin {
                default: 16,
                min: 1,
                max: 1024,
            },
        );
        assert_eq!(default_value(&hash), "16");
    }

    #[test]
    fn each_kind_shows_its_default() {
        let none = BTreeMap::new();
        let check = option("Ponder", UciOptionKind::Check { default: true });
        assert_eq!(value_for(&check, &none), "true");
        let combo = option(
            "Style",
            UciOptionKind::Combo {
                default: "Normal".into(),
                values: vec!["Solid".into(), "Normal".into()],
            },
        );
        assert_eq!(value_for(&combo, &none), "Normal");
        let text = option("SyzygyPath", UciOptionKind::String { default: "<empty>".into() });
        assert_eq!(value_for(&text, &none), "<empty>");
        let button = option("Clear Hash", UciOptionKind::Button);
        assert_eq!(value_for(&button, &none), "");
    }

    #[test]
    fn dialog_keeps_values_only_for_settable_options() {
        let options = vec![
            option("Ponder", UciOptionKind::Check { default: false }),
            option("Clear Hash", UciOptionKind::Button),
        ];
        let dialog = OptionsDialog::new(0, 'A', options, &BTreeMap::new());
        assert_eq!(dialog.values.len(), 1);
        assert!(dialog.values.contains_key("Ponder"));
        assert_eq!(dialog.title, "Engine A options");
        assert_eq!(dialog.slot(), 0);
    }
}
