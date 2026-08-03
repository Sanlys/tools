//! Profiles tab: list (household-visible), create, rename/delete/transfer
//! (owner-only -- the backend enforces this; this UI just doesn't offer the
//! buttons for a profile you don't own).

use game_mgr_api_types::{CreateProfileRequest, RenameProfileRequest, TransferProfileRequest};

use crate::{GameMgrPanel, Invalidate};

impl GameMgrPanel {
    pub(crate) fn ui_profiles(&mut self, ui: &mut egui::Ui) {
        let my_user_id = match self.me.ready() {
            Some(Ok(me)) => Some(me.user.id),
            _ => None,
        };

        ui.horizontal(|ui| {
            ui.label("New profile:");
            ui.text_edit_singleline(&mut self.new_profile_name);
            if ui.button("Create").clicked() && !self.new_profile_name.trim().is_empty() {
                let body = serde_json::to_value(CreateProfileRequest {
                    name: self.new_profile_name.trim().to_string(),
                })
                .unwrap_or_default();
                self.mutate("POST", "/api/v1/profiles", Some(body), Invalidate::Profiles);
                self.new_profile_name.clear();
            }
        });
        ui.separator();

        let profiles = match self.profiles.ready() {
            None => {
                ui.spinner();
                ui.label("loading profiles...");
                return;
            }
            Some(Err(err)) => {
                ui.colored_label(
                    egui::Color32::RED,
                    format!("failed to load profiles: {err}"),
                );
                return;
            }
            Some(Ok(profiles)) => profiles.clone(),
        };

        let users = self.users.ready().and_then(|r| r.as_ref().ok()).cloned();

        for p in &profiles {
            let owned = my_user_id == Some(p.owner_user_id);
            ui.group(|ui| {
                let is_renaming_this = matches!(&self.renaming, Some((id, _)) if *id == p.id);
                ui.horizontal(|ui| {
                    if is_renaming_this {
                        let (_, buf) = self.renaming.as_mut().unwrap();
                        ui.text_edit_singleline(buf);
                        if ui.button("Save").clicked() {
                            let (id, buf) = self.renaming.take().unwrap();
                            let name = buf.trim().to_string();
                            if !name.is_empty() {
                                let body = serde_json::to_value(RenameProfileRequest { name })
                                    .unwrap_or_default();
                                self.mutate(
                                    "PATCH",
                                    &format!("/api/v1/profiles/{id}"),
                                    Some(body),
                                    Invalidate::Profiles,
                                );
                            }
                        }
                        if ui.button("Cancel").clicked() {
                            self.renaming = None;
                        }
                    } else {
                        ui.label(egui::RichText::new(&p.name).strong());
                        if !owned {
                            ui.label(egui::RichText::new("(not yours)").weak());
                        }
                    }
                });

                if owned {
                    ui.horizontal(|ui| {
                        if ui.button("Rename").clicked() {
                            self.renaming = Some((p.id, p.name.clone()));
                        }
                        if ui.button("Delete").clicked() {
                            self.confirm_delete = Some(p.id);
                        }
                        // Exclude the profile's own current owner: transferring
                        // to yourself is a 422 from the backend anyway (see
                        // apps/game-mgr/backend/src/api/profiles.rs), and
                        // `self.mutate` is fire-and-forget, so previously
                        // picking it (the default, or an explicit selection)
                        // just silently did nothing visible.
                        let candidates: Vec<_> = users
                            .as_ref()
                            .map(|users| {
                                users
                                    .iter()
                                    .filter(|u| u.id != p.owner_user_id)
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_default();
                        if !candidates.is_empty() {
                            let target = self
                                .transfer_target
                                .entry(p.id)
                                .or_insert_with(|| candidates[0].id);
                            if !candidates.iter().any(|u| u.id == *target) {
                                *target = candidates[0].id;
                            }
                            let target_name = candidates
                                .iter()
                                .find(|u| u.id == *target)
                                .and_then(|u| u.display_name.clone().or(Some(u.sub.clone())))
                                .unwrap_or_default();
                            egui::ComboBox::from_id_salt(("transfer", p.id))
                                .selected_text(target_name)
                                .show_ui(ui, |ui| {
                                    for u in &candidates {
                                        let label =
                                            u.display_name.clone().unwrap_or_else(|| u.sub.clone());
                                        ui.selectable_value(target, u.id, label);
                                    }
                                });
                            if ui.button("Transfer").clicked() {
                                let to_user_id = *target;
                                let body =
                                    serde_json::to_value(TransferProfileRequest { to_user_id })
                                        .unwrap_or_default();
                                self.mutate(
                                    "POST",
                                    &format!("/api/v1/profiles/{}/transfer", p.id),
                                    Some(body),
                                    Invalidate::Profiles,
                                );
                            }
                        }
                    });

                    if self.confirm_delete == Some(p.id) {
                        ui.horizontal(|ui| {
                            ui.colored_label(
                                egui::Color32::RED,
                                "Delete this profile? This also deletes its sessions.",
                            );
                            if ui.button("Confirm delete").clicked() {
                                self.mutate(
                                    "DELETE",
                                    &format!("/api/v1/profiles/{}", p.id),
                                    None,
                                    Invalidate::Profiles,
                                );
                                self.confirm_delete = None;
                            }
                            if ui.button("Cancel").clicked() {
                                self.confirm_delete = None;
                            }
                        });
                    }
                }
            });
        }
    }
}
