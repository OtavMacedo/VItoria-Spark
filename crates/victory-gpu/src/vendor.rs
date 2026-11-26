//! Settings by card maker: `perfis/comum.toml` for every card, then the file of the maker of
//! the card the device is made on (`perfis/nvidia.toml`, `amd.toml`, `intel.toml`). Each sets
//! `VICTORY_*`/`SPARK_*` variables that the environment does not already set, so a person's
//! own settings always win. `SPARK_PERFIL=0` leaves the environment as it is.
//!
//! Applied once at start, before any setting is read: most are read once and kept.

/// The makers with a file of their own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Maker {
    Nvidia,
    Amd,
    Intel,
}

impl Maker {
    /// From a DXGI adapter's `VendorId`.
    pub fn from_vendor_id(id: u32) -> Option<Self> {
        match id {
            0x10de => Some(Maker::Nvidia),
            0x1002 | 0x1022 => Some(Maker::Amd),
            0x8086 => Some(Maker::Intel),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Maker::Nvidia => "nvidia",
            Maker::Amd => "amd",
            Maker::Intel => "intel",
        }
    }

    fn profile(self) -> &'static str {
        match self {
            Maker::Nvidia => include_str!("../../../perfis/nvidia.toml"),
            Maker::Amd => include_str!("../../../perfis/amd.toml"),
            Maker::Intel => include_str!("../../../perfis/intel.toml"),
        }
    }
}

const COMMON: &str = include_str!("../../../perfis/comum.toml");

/// The `[ajustes]` table of a profile as (variable, value) pairs. Only `VICTORY_*` and
/// `SPARK_*` names with string values are taken; a profile that does not parse gives none.
pub fn settings(profile: &str) -> Vec<(String, String)> {
    let Ok(table) = profile.parse::<toml::Table>() else {
        return Vec::new();
    };
    let Some(toml::Value::Table(set)) = table.get("ajustes") else {
        return Vec::new();
    };
    set.iter()
        .filter(|(k, _)| k.starts_with("VICTORY_") || k.starts_with("SPARK_"))
        .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_owned())))
        .collect()
}

/// The profile settings for `maker`, common ones first, the maker's own replacing them; less
/// those `is_set` says the environment already has.
pub fn plan(maker: Option<Maker>, is_set: impl Fn(&str) -> bool) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = settings(COMMON);
    for (k, v) in maker.map(|m| settings(m.profile())).unwrap_or_default() {
        match out.iter_mut().find(|(have, _)| *have == k) {
            Some(slot) => slot.1 = v,
            None => out.push((k, v)),
        }
    }
    out.retain(|(k, _)| !is_set(k));
    out
}

/// The maker of the card `D3D12CreateDevice(None, ..)` takes: DXGI's first adapter.
#[cfg(windows)]
pub fn detect() -> Option<(Maker, String)> {
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory2, DXGI_CREATE_FACTORY_FLAGS, IDXGIFactory4,
    };
    // SAFETY: plain DXGI queries on objects this function owns.
    let desc = unsafe {
        let factory: IDXGIFactory4 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)).ok()?;
        factory.EnumAdapters1(0).ok()?.GetDesc1().ok()?
    };
    let name = String::from_utf16_lossy(&desc.Description)
        .trim_end_matches('\0')
        .to_owned();
    Maker::from_vendor_id(desc.VendorId).map(|m| (m, name))
}

/// What [`apply`] did, to log once the session log is open.
#[derive(Debug, Default)]
pub struct Applied {
    pub maker: Option<Maker>,
    pub card: String,
    pub set: Vec<(String, String)>,
    pub off: bool,
}

impl Applied {
    pub fn log(&self) {
        if self.off {
            tracing::info!("perfis de placa desligados (SPARK_PERFIL=0)");
            return;
        }
        tracing::info!(
            spark.kind = "vendor_profile",
            maker = self.maker.map_or("desconhecido", Maker::name),
            card = %self.card,
            applied = %self.set.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" "),
            "perfil de placa aplicado"
        );
    }
}

/// Applies the profiles (see the module's comment). Call it first thing in `run`, before the
/// process makes other threads; log the result with [`Applied::log`].
#[cfg(windows)]
pub fn apply() -> Applied {
    if std::env::var("SPARK_PERFIL").is_ok_and(|v| v == "0") {
        return Applied {
            off: true,
            ..Applied::default()
        };
    }
    let found = detect();
    let maker = found.as_ref().map(|(m, _)| *m);
    let set = plan(maker, |k| std::env::var_os(k).is_some());
    for (k, v) in &set {
        // SAFETY: called before the process makes other threads (`run`'s first statement).
        unsafe { std::env::set_var(k, v) };
    }
    Applied {
        maker,
        card: found.map(|(_, n)| n).unwrap_or_default(),
        set,
        off: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_profile_parses_and_names_only_settings() {
        for m in [Maker::Nvidia, Maker::Amd, Maker::Intel] {
            assert!(m.profile().parse::<toml::Table>().is_ok(), "{}", m.name());
        }
        assert!(!settings(COMMON).is_empty());
        assert_eq!(
            settings("[ajustes]\nPATH = \"x\"\nVICTORY_A = 1\n"),
            Vec::new()
        );
    }

    #[test]
    fn the_maker_replaces_common_and_the_environment_wins() {
        let common = settings(COMMON);
        let (k, _) = common.first().expect("a common setting").clone();
        let plan_all = plan(None, |_| false);
        assert_eq!(plan_all.len(), common.len());
        let plan_set = plan(None, |name| name == k);
        assert!(plan_set.iter().all(|(name, _)| *name != k));
    }

    #[test]
    fn makers_by_vendor_id() {
        assert_eq!(Maker::from_vendor_id(0x10de), Some(Maker::Nvidia));
        assert_eq!(Maker::from_vendor_id(0x1002), Some(Maker::Amd));
        assert_eq!(Maker::from_vendor_id(0x8086), Some(Maker::Intel));
        assert_eq!(Maker::from_vendor_id(0x1414), None);
    }
}
