//! The upstream model catalog: every family audio.cpp knows, its tasks,
//! languages and options, and the downloadable packages for it.
//!
//! The catalog is upstream's `model_specs/*.json` at the pinned commit,
//! embedded at compile time, so it is available before any model is
//! downloaded. It describes what *can* be installed; [`crate::Registry::families`]
//! says what the linked library was built to *load*.
//!
//! This module does not download anything. [`Package::files`] and
//! [`Mirror::url`] give an application what its own downloader needs.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::task::{Mode, Task};

static BUNDLE: &str = include_str!("model_specs.json");

/// The embedded catalog.
#[derive(Debug, Clone, Deserialize)]
pub struct Catalog {
    /// Upstream commit the specs were taken from.
    pub source_commit: String,
    pub families: Vec<FamilySpec>,
}

impl Catalog {
    /// The catalog embedded in this crate, parsed once.
    pub fn builtin() -> &'static Catalog {
        static CATALOG: OnceLock<Catalog> = OnceLock::new();
        CATALOG.get_or_init(|| serde_json::from_str(BUNDLE).expect("embedded model catalog is valid"))
    }

    pub fn family(&self, family: &str) -> Option<&FamilySpec> {
        self.families.iter().find(|f| f.family == family)
    }

    /// The package with `id`, and the family it belongs to.
    pub fn package(&self, id: &str) -> Option<(&FamilySpec, &Package)> {
        self.families.iter().find_map(|f| f.packages.iter().find(|p| p.id == id).map(|p| (f, p)))
    }

    /// Families that serve `task` (in any mode).
    pub fn serving<'a>(&'a self, task: &'a Task) -> impl Iterator<Item = &'a FamilySpec> + 'a {
        self.families.iter().filter(move |f| f.tasks().contains(task))
    }
}

/// One model family, as described by its upstream spec.
#[derive(Debug, Clone, Deserialize)]
pub struct FamilySpec {
    pub family: String,
    pub display_name: String,
    pub description: String,
    /// `tts`, `asr`, `audio_tools`, `audio_generation`, `speech_analysis`, ...
    pub category: String,
    /// `supported`, `experimental`, `community`, `testing`, `wip`.
    pub status: String,
    /// Task names as the spec spells them; see [`Self::tasks`].
    #[serde(rename = "tasks")]
    pub spec_tasks: Vec<String>,
    #[serde(default)]
    pub modes: Vec<String>,
    #[serde(default)]
    pub languages: Vec<String>,
    /// Extra abilities per spec task, e.g. `clone: [speaker_reference]`.
    #[serde(default)]
    pub capabilities: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub options: OptionSpecs,
    #[serde(default)]
    pub packages: Vec<Package>,
    #[serde(default)]
    ui: Ui,
    #[serde(default)]
    package_defaults: PackageDefaults,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct Ui {
    #[serde(default)]
    recommended_package: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct PackageDefaults {
    #[serde(default)]
    download: Option<Download>,
}

impl FamilySpec {
    /// The family's tasks as [`Task`]s. Spec spellings differ from the ABI's
    /// (`music` is [`Task::Generation`], `clone` is [`Task::VoiceCloning`]).
    pub fn tasks(&self) -> Vec<Task> {
        let mut tasks: Vec<Task> = self.spec_tasks.iter().filter_map(|t| spec_task(t)).collect();
        tasks.dedup();
        tasks
    }

    pub fn supports_mode(&self, mode: Mode) -> bool {
        self.modes.iter().any(|m| m == mode.as_str())
    }

    /// UI tags such as `TTS`, `Clone`, `GGUF`.
    pub fn tags(&self) -> &[String] {
        &self.ui.tags
    }

    /// The package upstream recommends, else the one marked default, else the
    /// first downloadable one.
    pub fn recommended_package(&self) -> Option<&Package> {
        let downloadable = |p: &&Package| self.download(p).is_some_and(|d| d.is_downloadable());
        self.ui
            .recommended_package
            .as_deref()
            .and_then(|id| self.packages.iter().find(|p| p.id == id))
            .or_else(|| self.packages.iter().filter(downloadable).find(|p| p.default))
            .or_else(|| self.packages.iter().find(downloadable))
    }

    /// Where `package` downloads from: its own source, else the family default.
    pub fn download<'a>(&'a self, package: &'a Package) -> Option<&'a Download> {
        package.download.as_ref().or(self.package_defaults.download.as_ref())
    }
}

/// Maps a spec task name onto [`Task`] without calling into the library, so
/// the catalog works before `libaudiocpp` is loaded.
fn spec_task(name: &str) -> Option<Task> {
    Some(match name {
        "music" | "sfx" | "edit" | "audio_generation" | "gen" => Task::Generation,
        "clone" | "clon" => Task::VoiceCloning,
        "design" | "vdes" => Task::VoiceDesign,
        "speaker" | "spk" => Task::Speaker,
        other => Task::from(other),
    })
}

/// Options a family declares, grouped by where they go.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct OptionSpecs {
    #[serde(default)]
    pub request: Vec<OptionSpec>,
    #[serde(default)]
    pub session: Vec<OptionSpec>,
    #[serde(default)]
    pub load: Vec<OptionSpec>,
}

/// A declared option, typed for building UI controls.
#[derive(Debug, Clone, Deserialize)]
pub struct OptionSpec {
    pub name: String,
    /// `string`, `int`, `float`, `bool`, `enum`, `path`, `*_list`, ...
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub default: Option<serde_json::Value>,
    #[serde(default)]
    pub min: Option<serde_json::Value>,
    #[serde(default)]
    pub max: Option<serde_json::Value>,
    /// Allowed values for `enum` options.
    #[serde(default)]
    pub values: Vec<serde_json::Value>,
}

/// A downloadable set of files that together form one loadable model.
#[derive(Debug, Clone, Deserialize)]
pub struct Package {
    pub id: String,
    pub display_name: String,
    /// `gguf`, `safetensors`, ...
    pub format: String,
    /// `q8_0`, `bf16`, `f32`, `orig`, ...
    pub precision: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Directory, relative to the models root, the package installs into.
    /// Load the model from this directory.
    pub target_directory: String,
    /// Paths inside the remote repository.
    pub files: Vec<String>,
    #[serde(default)]
    pub strip_prefix: Option<String>,
    #[serde(default)]
    pub default: bool,
    #[serde(default)]
    download: Option<Download>,
}

/// One file to fetch: where it lives remotely and where it goes locally.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageFile {
    /// Path inside the remote repository.
    pub remote: String,
    /// Path relative to the models root: `target_directory/relative`.
    pub local: PathBuf,
    /// Path relative to the package's install directory, for applications
    /// that install each package into a directory of their own choosing.
    pub relative: PathBuf,
}

impl Package {
    /// The directory to load the model from, under `models_root`.
    pub fn install_dir(&self, models_root: &Path) -> Result<PathBuf> {
        Ok(models_root.join(safe_relative(&self.target_directory)?))
    }

    /// Every file of the package, with its local destination. Rejects specs
    /// whose paths would escape the models root.
    pub fn files(&self) -> Result<Vec<PackageFile>> {
        let target = safe_relative(&self.target_directory)?;
        self.files
            .iter()
            .map(|remote| {
                let relative = match self.strip_prefix.as_deref().map(|p| p.trim_end_matches('/')) {
                    Some(prefix) if !prefix.is_empty() => remote
                        .strip_prefix(prefix)
                        .and_then(|rest| rest.strip_prefix('/'))
                        .filter(|rest| !rest.is_empty())
                        .ok_or_else(|| {
                            Error::invalid_argument(format!("{}: {remote} does not start with {prefix}/", self.id))
                        })?,
                    _ => remote.as_str(),
                };
                let relative = safe_relative(relative)?;
                Ok(PackageFile { remote: remote.clone(), local: target.join(&relative), relative })
            })
            .collect()
    }
}

/// Rejects absolute paths and `..`, so a spec cannot write outside the root.
fn safe_relative(path: &str) -> Result<PathBuf> {
    let path = Path::new(path);
    if path.as_os_str().is_empty() || !path.components().all(|c| matches!(c, Component::Normal(_))) {
        return Err(Error::invalid_argument(format!("unsafe package path {}", path.display())));
    }
    Ok(path.to_path_buf())
}

/// How a package is distributed.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Download {
    /// Files from a Hugging Face model repository (mirrored on ModelScope for
    /// the main audio.cpp repository).
    HuggingfaceSnapshot {
        repo: String,
        #[serde(default = "main")]
        revision: String,
        #[serde(default)]
        gated: bool,
    },
    /// Files from a ModelScope model repository.
    ModelscopeSnapshot {
        repo: String,
        #[serde(default = "master")]
        revision: String,
    },
    /// No public download, e.g. a license forbids redistribution.
    Unsupported {
        #[serde(default)]
        reason: String,
    },
    #[serde(other)]
    Other,
}

fn main() -> String {
    "main".into()
}

fn master() -> String {
    "master".into()
}

impl Download {
    pub fn is_downloadable(&self) -> bool {
        matches!(self, Download::HuggingfaceSnapshot { .. } | Download::ModelscopeSnapshot { .. })
    }
}

/// Where to fetch files from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mirror {
    /// `endpoint` defaults to `https://huggingface.co`; `https://hf-mirror.com`
    /// is a common alternative.
    HuggingFace { endpoint: String },
    /// `endpoint` defaults to `https://www.modelscope.cn`.
    ModelScope { endpoint: String },
}

impl Default for Mirror {
    fn default() -> Self {
        Mirror::hugging_face()
    }
}

/// Hugging Face repositories with a known ModelScope mirror, and the
/// mirror's name.
const MODELSCOPE_MIRRORS: [(&str, &str); 1] = [("audio-cpp/audio.cpp-gguf", "HereIsMark/audio.cpp-gguf")];

/// The ModelScope mirror of a Hugging Face repository, if one is known.
pub fn modelscope_repo(huggingface_repo: &str) -> Option<&'static str> {
    MODELSCOPE_MIRRORS.iter().find(|(hf, _)| *hf == huggingface_repo).map(|(_, ms)| *ms)
}

impl Mirror {
    pub fn hugging_face() -> Self {
        Mirror::HuggingFace { endpoint: "https://huggingface.co".into() }
    }

    pub fn model_scope() -> Self {
        Mirror::ModelScope { endpoint: "https://www.modelscope.cn".into() }
    }

    /// The URL of `file` of a package distributed by `download`, or `None`
    /// when this mirror does not carry it.
    pub fn url(&self, download: &Download, file: &PackageFile) -> Option<String> {
        let path = file.remote.split('/').map(percent_encode).collect::<Vec<_>>().join("/");
        match (self, download) {
            (Mirror::HuggingFace { endpoint }, Download::HuggingfaceSnapshot { repo, revision, .. }) => {
                Some(format!("{}/{repo}/resolve/{}/{path}", endpoint.trim_end_matches('/'), percent_encode(revision)))
            }
            (Mirror::ModelScope { endpoint }, Download::HuggingfaceSnapshot { repo, revision, .. }) => {
                let mirror = modelscope_repo(repo)?;
                // ModelScope's default branch is master; only "main" maps to it.
                let revision = if revision == "main" { "master" } else { revision };
                Some(format!(
                    "{}/models/{mirror}/resolve/{}/{path}",
                    endpoint.trim_end_matches('/'),
                    percent_encode(revision)
                ))
            }
            (Mirror::ModelScope { endpoint }, Download::ModelscopeSnapshot { repo, revision }) => Some(format!(
                "{}/models/{repo}/resolve/{}/{path}",
                endpoint.trim_end_matches('/'),
                percent_encode(revision)
            )),
            _ => None,
        }
    }
}

fn percent_encode(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_catalog_parses_and_covers_known_families() {
        let catalog = Catalog::builtin();
        assert!(catalog.families.len() > 100);
        let qwen = catalog.family("qwen3_tts").unwrap();
        assert!(qwen.tasks().contains(&Task::VoiceCloning));
        assert!(qwen.tasks().contains(&Task::VoiceDesign));
        assert!(qwen.options.request.iter().any(|o| o.name == "temperature"));
        assert!(catalog.serving(&Task::Asr).count() > 10);
    }

    #[test]
    fn every_package_path_is_safe() {
        for family in &Catalog::builtin().families {
            for package in &family.packages {
                package.files().unwrap_or_else(|e| panic!("{}: {e}", package.id));
            }
        }
    }

    #[test]
    fn package_files_strip_prefix_into_target_directory() {
        let (_, package) = Catalog::builtin().package("qwen3_tts_0_6b_base_q8_0").unwrap();
        let files = package.files().unwrap();
        assert_eq!(files[0].remote, "Qwen3-TTS-12Hz-0.6B-Base-GGUF/qwen3-tts-12hz-0.6b-base-q8_0.gguf");
        assert_eq!(files[0].local, Path::new("Qwen3-TTS-12Hz-0.6B-Base-GGUF/qwen3-tts-12hz-0.6b-base-q8_0.gguf"));
        assert_eq!(files[0].relative, Path::new("qwen3-tts-12hz-0.6b-base-q8_0.gguf"));
    }

    #[test]
    fn mirrors_build_resolve_urls() {
        let (family, package) = Catalog::builtin().package("qwen3_tts_0_6b_base_q8_0").unwrap();
        let download = family.download(package).unwrap();
        let file = &package.files().unwrap()[0];
        assert_eq!(
            Mirror::hugging_face().url(download, file).unwrap(),
            "https://huggingface.co/audio-cpp/audio.cpp-gguf/resolve/main/Qwen3-TTS-12Hz-0.6B-Base-GGUF/qwen3-tts-12hz-0.6b-base-q8_0.gguf"
        );
        assert_eq!(
            Mirror::model_scope().url(download, file).unwrap(),
            "https://www.modelscope.cn/models/HereIsMark/audio.cpp-gguf/resolve/master/Qwen3-TTS-12Hz-0.6B-Base-GGUF/qwen3-tts-12hz-0.6b-base-q8_0.gguf"
        );
    }

    #[test]
    fn traversal_is_rejected() {
        assert!(safe_relative("../etc").is_err());
        assert!(safe_relative("/abs").is_err());
        assert!(safe_relative("a/b").is_ok());
    }
}
