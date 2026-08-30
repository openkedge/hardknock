// SPDX-License-Identifier: Apache-2.0
use super::{BundleId, SignedExperienceBundle};
use crate::{Error, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Default)]
pub struct FederationSelector {
    pub producer: Option<String>,
    pub task_family: Option<String>,
    pub marker: Option<String>,
    pub label: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FederationIndexEntry {
    pub bundle_id: BundleId,
    pub producer: String,
    pub object_types: Vec<String>,
    pub task_families: Vec<String>,
    pub repository_markers: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub schema_version: String,
    pub labels: Vec<String>,
    pub file: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct FederationIndex {
    schema: String,
    bundles: Vec<FederationIndexEntry>,
}
pub trait FederationTransport {
    fn publish(&self, bundle: &SignedExperienceBundle) -> Result<PathBuf>;
    fn fetch(&self, selector: &FederationSelector) -> Result<Vec<SignedExperienceBundle>>;
    fn search(&self, selector: &FederationSelector) -> Result<Vec<FederationIndexEntry>>;
}
pub struct FilesystemTransport {
    root: PathBuf,
    max_bundle_bytes: u64,
}
impl FilesystemTransport {
    pub fn new(root: &Path, max_bundle_bytes: u64) -> Result<Self> {
        if root.exists() && fs::symlink_metadata(root)?.file_type().is_symlink() {
            return Err(Error::Intervention(
                "Federation repository must not be a symlink".into(),
            ));
        }
        fs::create_dir_all(root)?;
        for child in ["manifests", "bundles", "peers"] {
            let p = root.join(child);
            if p.exists() && fs::symlink_metadata(&p)?.file_type().is_symlink() {
                return Err(Error::Intervention(
                    "Federation repository directories must not be symlinks".into(),
                ));
            }
            fs::create_dir_all(p)?;
        }
        Ok(Self {
            root: root.canonicalize()?,
            max_bundle_bytes,
        })
    }
    fn index_path(&self) -> PathBuf {
        self.root.join("index.json")
    }
    fn load_index(&self) -> Result<FederationIndex> {
        let path = self.index_path();
        if !path.exists() {
            return Ok(FederationIndex {
                schema: "hardknock.federation-index.v1".into(),
                bundles: vec![],
            });
        }
        let meta = fs::symlink_metadata(&path)?;
        if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > 10 * 1024 * 1024 {
            return Err(Error::InvalidInput(
                "Federation index is not a bounded regular file".into(),
            ));
        }
        Ok(serde_json::from_slice(&fs::read(path)?)?)
    }
    fn entry(bundle: &SignedExperienceBundle, file: String) -> FederationIndexEntry {
        let mut types = vec![];
        for (name, len) in [
            ("experience", bundle.bundle.experiences.len()),
            ("lesson", bundle.bundle.lessons.len()),
            ("skill", bundle.bundle.skills.len()),
            ("experiment", bundle.bundle.experiments.len()),
            ("reflex", bundle.bundle.reflexes.len()),
            ("recovery", bundle.bundle.recoveries.len()),
            ("envelope", bundle.bundle.envelopes.len()),
        ] {
            if len > 0 {
                types.push(name.into())
            }
        }
        let mut markers = Vec::new();
        let mut families = Vec::new();
        for context in bundle
            .bundle
            .lessons
            .iter()
            .map(|x| &x.context)
            .chain(bundle.bundle.skills.iter().map(|x| &x.context))
        {
            markers.extend(context.markers.clone());
            if let Some(f) = &context.repository_family {
                families.push(f.clone())
            }
        }
        markers.sort();
        markers.dedup();
        families.sort();
        families.dedup();
        FederationIndexEntry {
            bundle_id: bundle.manifest.bundle_id.clone(),
            producer: bundle.manifest.producer.to_string(),
            object_types: types,
            task_families: families,
            repository_markers: markers,
            created_at: bundle.manifest.created_at,
            schema_version: bundle.manifest.schema_version.clone(),
            labels: bundle.manifest.labels.clone(),
            file,
        }
    }
}
impl FederationTransport for FilesystemTransport {
    fn publish(&self, bundle: &SignedExperienceBundle) -> Result<PathBuf> {
        let digest = bundle
            .manifest
            .bundle_id
            .to_string()
            .trim_start_matches("hk-bundle:")
            .to_owned();
        let relative = format!("bundles/{digest}.hkexp");
        let destination = self.root.join(&relative);
        let bytes = serde_json::to_vec_pretty(bundle)?;
        if bytes.len() as u64 > self.max_bundle_bytes {
            return Err(Error::InvalidInput("Bundle size limit exceeded".into()));
        }
        if destination.exists() {
            let existing = fs::read(&destination)?;
            if serde_json::from_slice::<SignedExperienceBundle>(&existing)?.payload_hash
                == bundle.payload_hash
            {
                return Ok(destination);
            }
            return Err(Error::InvalidInput(
                "Content-addressed bundle path contains different data".into(),
            ));
        }
        let mut options = OpenOptions::new();
        options.write(true).create_new(true).mode(0o600);
        let mut file = options.open(&destination)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        let mut index = self.load_index()?;
        index.bundles.push(Self::entry(bundle, relative));
        index.bundles.sort_by(|a, b| a.bundle_id.cmp(&b.bundle_id));
        index.bundles.dedup_by(|a, b| a.bundle_id == b.bundle_id);
        let temp = self.root.join(format!(".index-{}.tmp", std::process::id()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true).mode(0o600);
        let mut output = options.open(&temp)?;
        serde_json::to_writer_pretty(&mut output, &index)?;
        output.sync_all()?;
        fs::rename(temp, self.index_path())?;
        Ok(destination)
    }
    fn fetch(&self, selector: &FederationSelector) -> Result<Vec<SignedExperienceBundle>> {
        self.search(selector)?
            .into_iter()
            .map(|entry| {
                let path = self.root.join(&entry.file);
                if !path.starts_with(&self.root) {
                    return Err(Error::InvalidInput(
                        "Federation index path traversal rejected".into(),
                    ));
                }
                let meta = fs::symlink_metadata(&path)?;
                if !meta.is_file()
                    || meta.file_type().is_symlink()
                    || meta.len() > self.max_bundle_bytes
                {
                    return Err(Error::InvalidInput(
                        "Federation bundle is not a bounded regular file".into(),
                    ));
                }
                let mut bytes = Vec::new();
                fs::File::open(path)?
                    .take(self.max_bundle_bytes + 1)
                    .read_to_end(&mut bytes)?;
                if bytes.len() as u64 > self.max_bundle_bytes {
                    return Err(Error::InvalidInput("Bundle size limit exceeded".into()));
                }
                Ok(serde_json::from_slice(&bytes)?)
            })
            .collect()
    }
    fn search(&self, selector: &FederationSelector) -> Result<Vec<FederationIndexEntry>> {
        Ok(self
            .load_index()?
            .bundles
            .into_iter()
            .filter(|e| {
                selector.producer.as_ref().is_none_or(|p| &e.producer == p)
                    && selector
                        .task_family
                        .as_ref()
                        .is_none_or(|p| e.task_families.contains(p))
                    && selector
                        .marker
                        .as_ref()
                        .is_none_or(|p| e.repository_markers.contains(p))
                    && selector.label.as_ref().is_none_or(|p| e.labels.contains(p))
            })
            .collect())
    }
}
