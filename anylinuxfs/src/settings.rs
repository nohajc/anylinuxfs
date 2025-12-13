use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    env,
    fmt::Display,
    fs,
    net::IpAddr,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
};

use anyhow::{Context, anyhow};
use bstr::{BString, ByteSlice, ByteVec};
use clap::ValueEnum;
use common_utils::{CustomActionConfig, CustomActionConfigOld, OSType};
use serde::{Deserialize, Serialize};
use toml_edit::{Document, DocumentMut, Item};

#[cfg(feature = "freebsd")]
use crate::vm_image::KERNEL_IMAGE;

use crate::utils;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct KernelConfig {
    pub os: OSType,
    pub path: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Config {
    pub home_dir: PathBuf,
    pub profile_path: PathBuf,
    pub exec_path: PathBuf,
    pub root_path: PathBuf,
    pub root_ver_file_path: PathBuf,
    pub config_file_path: PathBuf,
    pub log_file_path: PathBuf,
    pub libexec_path: PathBuf,
    pub init_rootfs_path: PathBuf,
    pub kernel: KernelConfig,
    pub gvproxy_net_sock_path: String,
    pub gvproxy_path: PathBuf,
    pub gvproxy_log_path: PathBuf,
    pub vmproxy_host_path: PathBuf,
    pub vsock_path: String,
    pub vfkit_sock_path: String,
    pub invoker_uid: libc::uid_t,
    pub invoker_gid: libc::gid_t,
    pub sudo_uid: Option<libc::uid_t>,
    pub sudo_gid: Option<libc::gid_t>,
    pub passphrase_config: PassphrasePromptConfig,
    #[cfg(feature = "freebsd")]
    pub zfs_os: OSType,
    pub preferences: [PrefsObject; 2],
}

impl Config {
    #[cfg(feature = "freebsd")]
    pub fn with_image_source(&self, src: &ImageSource) -> Self {
        let mut new_config = self.clone();
        new_config.kernel.os = src.os_type;

        if src.os_type != OSType::Linux {
            let kernel_path = self.profile_path.join(&src.base_dir).join(KERNEL_IMAGE);
            new_config.kernel.path = kernel_path;
        }
        new_config
    }
}

pub trait Preferences {
    fn alpine_custom_packages<'a>(&'a self) -> BTreeSet<&'a str>;
    fn custom_actions<'a>(&'a self) -> BTreeMap<&'a str, &'a CustomActionConfig>;
    #[cfg(feature = "freebsd")]
    fn images<'a>(&'a self) -> BTreeMap<&'a str, &'a ImageSource>;
    fn gvproxy_debug(&self) -> bool;
    fn krun_log_level_numeric(&self) -> u32;
    fn krun_num_vcpus(&self) -> u8;
    fn krun_ram_size_mib(&self) -> u32;
    fn passphrase_prompt_config(&self) -> PassphrasePromptConfig;
    #[cfg(feature = "freebsd")]
    fn default_image(&self, os_type: OSType) -> Option<&str>;
    #[cfg(feature = "freebsd")]
    fn zfs_os(&self) -> OSType;

    fn user<'a>(&'a self) -> &'a PrefsObject;
    fn user_mut<'a>(&'a mut self) -> &'a mut PrefsObject;
    // fn global<'a>(&'a self) -> &'a PrefsObject;
    // fn global_mut<'a>(&'a mut self) -> &'a mut PrefsObject;

    fn merged(&self) -> PrefsObject;
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct PrefsObject {
    #[serde(default)]
    pub alpine: AlpineConfig,
    #[serde(default)]
    pub custom_actions: BTreeMap<String, CustomActionConfig>,
    #[serde(default)]
    pub images: BTreeMap<String, ImageSource>,
    #[serde(default)]
    pub gvproxy: GvproxyConfig,
    #[serde(default)]
    pub krun: KrunConfig,
    #[serde(default)]
    pub misc: MiscConfig,
    #[serde(default)]
    pub linux: OSConfig,
    #[serde(default)]
    pub freebsd: OSConfig,
    // legacy config
    #[serde(rename = "log_level")]
    pub log_level_numeric: Option<u32>,
    pub num_vcpus: Option<u8>,
    pub ram_size_mib: Option<u32>,
}

impl Preferences for [PrefsObject; 2] {
    fn alpine_custom_packages<'a>(&'a self) -> BTreeSet<&'a str> {
        let mut result =
            BTreeSet::from_iter(self[0].alpine.custom_packages.iter().map(|s| s.as_str()));
        result.extend(self[1].alpine.custom_packages.iter().map(|s| s.as_str()));
        result
    }

    fn custom_actions<'a>(&'a self) -> BTreeMap<&'a str, &'a CustomActionConfig> {
        let mut result: BTreeMap<_, _> = self[0]
            .custom_actions
            .iter()
            .map(|(k, v)| (k.as_str(), v))
            .collect();
        result.extend(self[1].custom_actions.iter().map(|(k, v)| (k.as_str(), v)));
        result
    }

    #[cfg(feature = "freebsd")]
    fn images<'a>(&'a self) -> BTreeMap<&'a str, &'a ImageSource> {
        let mut result: BTreeMap<_, _> = self[0]
            .images
            .iter()
            .map(|(k, v)| (k.as_str(), v))
            .collect();
        result.extend(self[1].images.iter().map(|(k, v)| (k.as_str(), v)));
        result
    }

    fn gvproxy_debug(&self) -> bool {
        self[1]
            .gvproxy
            .debug
            .or(self[0].gvproxy.debug)
            .unwrap_or(false)
    }

    fn krun_log_level_numeric(&self) -> u32 {
        self[1]
            .krun
            .log_level_numeric
            .or(self[0].krun.log_level_numeric)
            .unwrap_or(KrunConfig::default_log_level())
    }

    fn krun_num_vcpus(&self) -> u8 {
        self[1]
            .krun
            .num_vcpus
            .or(self[0].krun.num_vcpus)
            .unwrap_or(KrunConfig::default_num_vcpus())
    }

    fn krun_ram_size_mib(&self) -> u32 {
        self[1]
            .krun
            .ram_size_mib
            .or(self[0].krun.ram_size_mib)
            .unwrap_or(KrunConfig::default_ram_size())
    }

    fn passphrase_prompt_config(&self) -> PassphrasePromptConfig {
        self[1]
            .misc
            .passphrase_config
            .or(self[0].misc.passphrase_config)
            .unwrap_or_default()
    }

    #[cfg(feature = "freebsd")]
    fn default_image(&self, os_type: OSType) -> Option<&str> {
        match os_type {
            OSType::Linux => self[1].linux.default_image.as_deref(),
            OSType::FreeBSD => self[1].freebsd.default_image.as_deref(),
        }
    }

    #[cfg(feature = "freebsd")]
    fn zfs_os(&self) -> OSType {
        self[1]
            .misc
            .zfs_os
            .or(self[0].misc.zfs_os)
            .unwrap_or_default()
    }

    fn user<'a>(&'a self) -> &'a PrefsObject {
        &self[1]
    }

    fn user_mut<'a>(&'a mut self) -> &'a mut PrefsObject {
        &mut self[1]
    }

    // fn global<'a>(&'a self) -> &'a PrefsObject {
    //     &self[0]
    // }

    // fn global_mut<'a>(&'a mut self) -> &'a mut PrefsObject {
    //     &mut self[0]
    // }

    fn merged(&self) -> PrefsObject {
        let result = self[0].clone();
        result.merge_with(&self[1])
    }
}

impl PrefsObject {
    pub fn merge_with(&self, other: &PrefsObject) -> PrefsObject {
        let mut custom_actions = self.custom_actions.clone();
        custom_actions.extend(other.custom_actions.clone());

        let mut images = self.images.clone();
        images.extend(other.images.clone());

        PrefsObject {
            alpine: self.alpine.merge_with(&other.alpine),
            custom_actions,
            images,
            gvproxy: self.gvproxy.merge_with(&other.gvproxy),
            krun: self.krun.merge_with(&other.krun),
            misc: self.misc.merge_with(&other.misc),
            linux: self.linux.merge_with(&other.linux),
            freebsd: self.freebsd.merge_with(&other.freebsd),
            log_level_numeric: other.log_level_numeric.or(self.log_level_numeric),
            num_vcpus: other.num_vcpus.or(self.num_vcpus),
            ram_size_mib: other.ram_size_mib.or(self.ram_size_mib),
        }
    }
}

impl Display for PrefsObject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[krun]\n{}", self.krun)?;
        write!(f, "\n\n[misc]\n{}", self.misc)?;
        Ok(())
    }
}

fn convert_legacy_config(config: &mut PrefsObject) {
    if let Some(log_level_numeric) = config.log_level_numeric.take() {
        config.krun.log_level_numeric = Some(log_level_numeric);
    }
    if let Some(num_vcpus) = config.num_vcpus.take() {
        config.krun.num_vcpus = Some(num_vcpus);
    }
    if let Some(ram_size_mib) = config.ram_size_mib.take() {
        config.krun.ram_size_mib = Some(ram_size_mib);
    }
}

// FIXME: remove at some point in the future
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct PrefsObjectOld {
    #[serde(default)]
    pub alpine: AlpineConfig,
    #[serde(default)]
    pub custom_actions: BTreeMap<String, CustomActionConfigOld>,
    #[serde(default)]
    pub images: BTreeMap<String, ImageSource>,
    #[serde(default)]
    pub gvproxy: GvproxyConfig,
    #[serde(default)]
    pub krun: KrunConfig,
    #[serde(default)]
    pub misc: MiscConfig,
    // legacy config
    #[serde(rename = "log_level")]
    pub log_level_numeric: Option<u32>,
    pub num_vcpus: Option<u8>,
    pub ram_size_mib: Option<u32>,
}

impl From<PrefsObjectOld> for PrefsObject {
    fn from(value: PrefsObjectOld) -> Self {
        PrefsObject {
            alpine: value.alpine,
            custom_actions: value
                .custom_actions
                .into_iter()
                .map(|(k, v)| (k, v.into()))
                .collect(),
            images: value.images,
            gvproxy: value.gvproxy,
            krun: value.krun,
            misc: value.misc,
            linux: OSConfig::default(),
            freebsd: OSConfig::default(),
            log_level_numeric: value.log_level_numeric,
            num_vcpus: value.num_vcpus,
            ram_size_mib: value.ram_size_mib,
        }
    }
}

pub fn load_preferences<'a>(
    paths: impl Iterator<Item = &'a Path>,
) -> anyhow::Result<[PrefsObject; 2]> {
    let mut result_config = [PrefsObject::default(), PrefsObject::default()];
    let mut cfg_idx = 0;
    for path in paths {
        match fs::read_to_string(path) {
            Ok(config_str) => {
                let mut config = parse_toml_config(&config_str, path)
                    .context(format!("Failed to parse config file {}", path.display()))?;
                convert_legacy_config(&mut config);
                result_config[cfg_idx] = config;
            }
            Err(_) => (),
        };
        cfg_idx += 1;
    }
    Ok(result_config)
}

pub fn parse_toml_config(config_str: &str, path: impl AsRef<Path>) -> anyhow::Result<PrefsObject> {
    let res = match toml::from_str::<PrefsObject>(config_str) {
        Ok(config) => Ok(config),
        Err(_) => {
            // this could be a config affected by the byte string serialization bug
            let config: PrefsObjectOld = toml::from_str(config_str)
                .context("Failed to parse Preferences from TOML string")?;

            // let's fix it up
            let fixed_config = config.into();
            save_preferences(&fixed_config, path.as_ref())
                .context("Error while converting config to the latest format")?;

            Ok(fixed_config)
        }
    };
    res
}

pub fn save_preferences(preferences: &PrefsObject, config_file_path: &Path) -> anyhow::Result<()> {
    let config_str =
        toml::to_string(preferences).context("Failed to serialize Preferences to TOML")?;
    // println!(
    //     "Saving config to {}:\n{}",
    //     config_file_path.display(),
    //     config_str
    // );
    fs::write(config_file_path, config_str).context(format!(
        "Failed to write config file {}",
        config_file_path.display()
    ))?;
    Ok(())
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct AlpineConfig {
    pub custom_packages: Vec<String>,
}

impl AlpineConfig {
    fn merge_with(&self, other: &AlpineConfig) -> AlpineConfig {
        let mut custom_packages = BTreeSet::from_iter(self.custom_packages.clone());
        custom_packages.extend(other.custom_packages.clone());
        AlpineConfig {
            custom_packages: custom_packages.into_iter().collect(),
        }
    }
}

pub trait CustomActionEnvironment {
    fn prepare_environment(&self, env_vars: &mut Vec<BString>) -> anyhow::Result<()>;
}

impl CustomActionEnvironment for CustomActionConfig {
    fn prepare_environment(&self, env_vars: &mut Vec<BString>) -> anyhow::Result<()> {
        let mut referenced_variables = HashSet::new();
        for script in self.all_scripts() {
            referenced_variables.extend(utils::find_env_vars(script));
        }

        let mut predefined_vars = HashSet::new();
        let mut undefined_vars = Vec::new();

        for var_str in self.environment() {
            let var_name = var_str
                .split(|&c| c == b'=')
                .next()
                .ok_or_else(|| anyhow!("invalid environment variable format: {}", var_str))?;
            predefined_vars.insert(BString::from(var_name));
            env_vars.push(var_str.to_owned());
        }
        for var_name in referenced_variables
            .iter()
            .map(|e| e.as_bstr())
            .chain(self.capture_environment())
        {
            match env::var_os(var_name.to_os_str_lossy()) {
                Some(var_value) => {
                    let mut var_str = var_name.to_owned();
                    var_str.push_str(b"=");
                    var_str.push_str(var_value.as_bytes());
                    env_vars.push(var_str);
                }
                None => {
                    if !Self::VM_EXPORTED_VARS.contains(&var_name.as_bytes())
                        && !predefined_vars.contains(var_name)
                    {
                        undefined_vars.push(var_name);
                    }
                }
            }
        }

        if !undefined_vars.is_empty() {
            let var_list = bstr::join(", ", undefined_vars);
            return Err(anyhow::anyhow!(
                "required environment variables not defined: {}",
                var_list.as_bstr()
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ImageSource {
    pub base_dir: String,
    pub docker_ref: Option<String>,
    pub iso_url: Option<String>,
    pub oci_url: Option<String>,
    pub kernel: KernelSource,
    pub os_type: OSType,
}

impl ImageSource {
    #[cfg(feature = "freebsd")]
    pub fn installed_in(&self, profile_path: impl AsRef<Path>) -> bool {
        profile_path
            .as_ref()
            .join(&self.base_dir)
            .join("rootfs.ver")
            .exists()
    }
}

impl Default for ImageSource {
    fn default() -> Self {
        ImageSource {
            base_dir: "alpine".into(),
            docker_ref: Some("alpine:latest".into()),
            iso_url: None,
            oci_url: None,
            kernel: KernelSource::default(),
            os_type: OSType::Linux,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct KernelSource {
    pub bundle_url: Option<String>,
    pub image_url: Option<String>,
    pub modules_url: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct GvproxyConfig {
    pub debug: Option<bool>,
}

impl GvproxyConfig {
    fn merge_with(&self, other: &GvproxyConfig) -> GvproxyConfig {
        GvproxyConfig {
            debug: other.debug.or(self.debug),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct KrunConfig {
    #[serde(rename = "log_level")]
    pub log_level_numeric: Option<u32>,
    pub num_vcpus: Option<u8>,
    pub ram_size_mib: Option<u32>,
}

impl KrunConfig {
    fn default_log_level() -> u32 {
        0
    }

    fn default_num_vcpus() -> u8 {
        1
    }

    fn default_ram_size() -> u32 {
        1152
    }

    fn merge_with(&self, other: &KrunConfig) -> KrunConfig {
        KrunConfig {
            log_level_numeric: other.log_level_numeric.or(self.log_level_numeric),
            num_vcpus: other.num_vcpus.or(self.num_vcpus),
            ram_size_mib: other.ram_size_mib.or(self.ram_size_mib),
        }
    }
}

impl Display for KrunConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "log_level = {}\nnum_vcpus = {}\nram_size_mib = {}",
            self.log_level(),
            self.num_vcpus.unwrap_or(KrunConfig::default_num_vcpus()),
            self.ram_size_mib.unwrap_or(KrunConfig::default_ram_size())
        )
    }
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum KrunLogLevel {
    Off = 0,
    Error = 1,
    Warn = 2,
    Info = 3,
    Debug = 4,
    Trace = 5,
}

impl Display for KrunLogLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let val = match self {
            KrunLogLevel::Off => "off",
            KrunLogLevel::Error => "error",
            KrunLogLevel::Warn => "warn",
            KrunLogLevel::Info => "info",
            KrunLogLevel::Debug => "debug",
            KrunLogLevel::Trace => "trace",
        };
        write!(f, "{}", val)
    }
}

impl From<u32> for KrunLogLevel {
    fn from(value: u32) -> Self {
        match value {
            0 => KrunLogLevel::Off,
            1 => KrunLogLevel::Error,
            2 => KrunLogLevel::Warn,
            3 => KrunLogLevel::Info,
            4 => KrunLogLevel::Debug,
            5 => KrunLogLevel::Trace,
            _ => KrunLogLevel::Off,
        }
    }
}

#[allow(unused)]
impl KrunConfig {
    pub fn log_level(&self) -> KrunLogLevel {
        self.log_level_numeric
            .unwrap_or(KrunConfig::default_log_level())
            .into()
    }

    pub fn set_log_level(&mut self, level: KrunLogLevel) {
        self.log_level_numeric = Some(level as u32);
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct MiscConfig {
    pub passphrase_config: Option<PassphrasePromptConfig>,
    pub zfs_os: Option<OSType>,
}

impl MiscConfig {
    fn merge_with(&self, other: &MiscConfig) -> MiscConfig {
        MiscConfig {
            passphrase_config: other.passphrase_config.or(self.passphrase_config.clone()),
            zfs_os: other.zfs_os.or(self.zfs_os),
        }
    }

    fn passphrase_config(&self) -> PassphrasePromptConfig {
        self.passphrase_config.unwrap_or_default()
    }
}

impl Display for MiscConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "passphrase_config = {}\nzfs_os = {:?}",
            self.passphrase_config(),
            self.zfs_os.unwrap_or_default()
        )
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct OSConfig {
    pub default_image: Option<String>,
}
impl OSConfig {
    fn merge_with(&self, other: &OSConfig) -> OSConfig {
        OSConfig {
            default_image: other.default_image.clone().or(self.default_image.clone()),
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MountConfig {
    pub disk_path: String,
    pub read_only: bool,
    pub mount_options: Option<String>,
    pub allow_remount: bool,
    pub custom_mount_point: Option<PathBuf>,
    pub fs_driver: Option<String>,
    pub bind_addr: IpAddr,
    pub verbose: bool,
    pub open_finder: bool,
    pub common: Config,
    pub custom_action: Option<String>,
}

impl MountConfig {
    pub fn get_action(&self) -> Option<&CustomActionConfig> {
        match self.custom_action.as_deref() {
            Some(action_name) => self
                .common
                .preferences
                .custom_actions()
                .get(action_name)
                .map(|a| *a),
            None => None,
        }
    }

    #[cfg(feature = "freebsd")]
    pub fn with_image_source(&self, src: &ImageSource) -> Self {
        let mut new_config = self.clone();
        new_config.common = new_config.common.with_image_source(src);
        new_config
    }
}

#[derive(Debug, Clone, Copy, ValueEnum, Deserialize, Serialize, PartialEq, Eq)]
pub enum PassphrasePromptConfig {
    #[clap(name = "a")]
    #[serde(rename = "ask_for_each")]
    AskForEach,
    #[clap(name = "1")]
    #[serde(rename = "one_for_all")]
    OneForAll,
}

impl Default for PassphrasePromptConfig {
    fn default() -> Self {
        PassphrasePromptConfig::AskForEach
    }
}

impl Display for PassphrasePromptConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let val = match self {
            PassphrasePromptConfig::AskForEach => "ask_for_each",
            PassphrasePromptConfig::OneForAll => "one_for_all",
        };
        write!(f, "{}", val)
    }
}

pub fn merge_toml_configs<S>(dst: &mut DocumentMut, src: &Document<S>) -> anyhow::Result<()> {
    for (key, dst_item) in dst.iter_mut() {
        if let Some(src_item) = src.get(&key) {
            match dst_item {
                Item::None => (),
                Item::Value(value) => {
                    *value = src_item
                        .as_value()
                        .context("source item is not a value")?
                        .to_owned();
                }
                Item::Table(table) => {
                    merge_toml_tables(
                        table,
                        src_item.as_table().context("source item is not a table")?,
                    );
                }
                Item::ArrayOfTables(_array_of_tables) => unimplemented!(),
            }
        }
    }

    merge_toml_tables(dst, src.as_table());
    Ok(())
}

fn merge_toml_tables(dst: &mut toml_edit::Table, src: &toml_edit::Table) {
    let decor = toml_edit::Decor::new("\n", "");
    for (key, src_item) in src.iter() {
        // only add src items not present in dst
        if dst.contains_key(key) {
            continue;
        }
        let item = match src_item {
            Item::Table(tbl) => Item::Table(tbl.without_pos(decor.clone())),
            _ => src_item.clone(),
        };
        dst.insert(&key, item);
    }
}

trait TableExt {
    fn without_pos(&self, decor: toml_edit::Decor) -> Self;
}

impl TableExt for toml_edit::Table {
    fn without_pos(&self, decor: toml_edit::Decor) -> Self {
        let mut new_table = Self::default();

        *new_table.decor_mut() = decor;
        new_table.set_implicit(self.is_implicit());
        new_table.set_dotted(self.is_dotted());
        new_table.extend(self);

        new_table
    }
}
