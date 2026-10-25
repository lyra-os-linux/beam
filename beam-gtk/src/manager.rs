//! Conexões salvas e certificados confiáveis, com as mesmas regras da antiga
//! interface GTK: a lista é gravada inteira de forma atômica e a credencial
//! do chaveiro só é apagada quando nenhuma outra conexão a usa.

use std::sync::Mutex;

use beam_core::profile::{self, ConnectionProfile};
use beam_core::{known_hosts, secrets};
use serde::Serialize;
use uuid::Uuid;

use crate::i18n::gettext;

/// Cópia em memória do `connections.toml`, como a lista do GTK.
#[derive(Default)]
pub struct Profiles(Mutex<Vec<ConnectionProfile>>);

#[derive(Serialize)]
pub struct ProfileList {
    profiles: Vec<ConnectionProfile>,
    /// Falha ao ler o arquivo: a interface mostra o erro e começa vazia.
    load_error: Option<String>,
}

fn same_credential(a: &ConnectionProfile, b: &ConnectionProfile) -> bool {
    a.normalized_host() == b.normalized_host() && a.port == b.port && a.username == b.username
}

async fn forget_credential(profile: &ConnectionProfile) {
    let key = secrets::SecretKey {
        host: profile.normalized_host(),
        port: profile.port,
        user: &profile.username,
    };
    if let Err(error) = secrets::delete_password(&key).await {
        tracing::warn!(%error, "falha ao remover credencial sem uso");
    }
}

fn save(list: &[ConnectionProfile]) -> Result<(), String> {
    profile::save_profiles(list).map_err(|error| {
        tracing::error!(%error, "falha ao salvar conexões");
        format!("{}: {error}", gettext("Could not save connections"))
    })
}

#[tauri::command]
pub fn profiles_list(state: tauri::State<'_, Profiles>) -> ProfileList {
    let (loaded, load_error) = match profile::load_profiles() {
        Ok(profiles) => (profiles, None),
        Err(error) => (Vec::new(), Some(error.to_string())),
    };
    *state.0.lock().unwrap() = loaded.clone();
    ProfileList {
        profiles: loaded,
        load_error,
    }
}

/// Valida o que o diálogo do GTK garantia pelos próprios controles.
fn validate(profile: &ConnectionProfile) -> Result<(), String> {
    let resolution = profile.resolution;
    if profile.name.trim().is_empty() || profile.host.trim().is_empty() {
        return Err(gettext("Fill in the connection name and the server."));
    }
    if profile.port == 0
        || !(320..=7680).contains(&resolution.width)
        || !(240..=4320).contains(&resolution.height)
        || !matches!(profile.color_depth, 16 | 32)
    {
        return Err(gettext("Some connection values are out of range."));
    }
    Ok(())
}

/// Cria (id novo) ou atualiza uma conexão. Ao trocar servidor, porta ou
/// usuário, a senha antiga sai do chaveiro se nenhuma conexão a usa mais.
#[tauri::command]
pub async fn profile_save(
    state: tauri::State<'_, Profiles>,
    mut profile: ConnectionProfile,
) -> Result<ConnectionProfile, String> {
    profile.name = profile.name.trim().to_owned();
    profile.host = profile.host.trim().to_owned();
    profile.domain = profile
        .domain
        .map(|d| d.trim().to_owned())
        .filter(|d| !d.is_empty());
    validate(&profile)?;
    let stale = {
        let mut list = state.0.lock().unwrap();
        let mut saved = list.clone();
        let previous = match saved.iter().position(|p| p.id == profile.id) {
            Some(position) => Some(std::mem::replace(&mut saved[position], profile.clone())),
            None => {
                profile.id = Uuid::new_v4();
                saved.push(profile.clone());
                None
            }
        };
        save(&saved)?;
        let stale = previous.filter(|previous| {
            !same_credential(previous, &profile)
                && !saved.iter().any(|p| same_credential(p, previous))
        });
        *list = saved;
        stale
    };
    if let Some(previous) = stale {
        forget_credential(&previous).await;
    }
    Ok(profile)
}

#[tauri::command]
pub fn profile_duplicate(
    state: tauri::State<'_, Profiles>,
    id: Uuid,
) -> Result<ConnectionProfile, String> {
    let mut list = state.0.lock().unwrap();
    let source = list
        .iter()
        .find(|p| p.id == id)
        .ok_or_else(|| gettext("Connection not found."))?;
    let copy = source.duplicate(&gettext("copy"));
    let mut saved = list.clone();
    saved.push(copy.clone());
    save(&saved)?;
    *list = saved;
    Ok(copy)
}

#[tauri::command]
pub async fn profile_delete(state: tauri::State<'_, Profiles>, id: Uuid) -> Result<(), String> {
    let target = {
        let mut list = state.0.lock().unwrap();
        let Some(target) = list.iter().find(|p| p.id == id).cloned() else {
            return Ok(());
        };
        let mut updated = list.clone();
        updated.retain(|p| p.id != id);
        save(&updated)?;
        let still_used = updated.iter().any(|p| same_credential(p, &target));
        *list = updated;
        (!still_used).then_some(target)
    };
    if let Some(target) = target {
        forget_credential(&target).await;
    }
    Ok(())
}

pub fn find(state: &Profiles, id: Uuid) -> Option<ConnectionProfile> {
    state.0.lock().unwrap().iter().find(|p| p.id == id).cloned()
}

#[derive(Serialize)]
pub struct TrustedHost {
    address: String,
    fingerprint: String,
}

#[tauri::command]
pub fn trusted_list() -> Result<Vec<TrustedHost>, String> {
    let mut hosts: Vec<TrustedHost> = known_hosts::list()
        .map_err(|error| {
            format!(
                "{}: {error}",
                gettext("Could not load trusted certificates")
            )
        })?
        .into_iter()
        .map(|(address, fingerprint)| TrustedHost {
            address,
            fingerprint: fingerprint.as_str().to_owned(),
        })
        .collect();
    hosts.sort_by(|a, b| a.address.cmp(&b.address));
    Ok(hosts)
}

#[tauri::command]
pub fn trusted_remove(address: String) -> Result<(), String> {
    known_hosts::remove(&address).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_identity_ignores_ipv6_brackets() {
        let a = ConnectionProfile::new("a", "[::1]", "ana");
        let b = ConnectionProfile::new("b", "::1", "ana");
        assert!(same_credential(&a, &b));
        let mut c = b.clone();
        c.port = 3390;
        assert!(!same_credential(&a, &c));
    }

    #[test]
    fn validation_matches_the_former_dialog_limits() {
        let mut p = ConnectionProfile::new("Office", "host", "user");
        assert!(validate(&p).is_ok());
        p.resolution.width = 100;
        assert!(validate(&p).is_err());
        p.resolution.width = 1920;
        p.color_depth = 24;
        assert!(validate(&p).is_err());
        p.color_depth = 32;
        p.host = "  ".into();
        assert!(validate(&p).is_err());
    }
}
