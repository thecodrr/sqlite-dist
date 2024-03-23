use std::{
    io::{self, Cursor, Write},
    path::Path,
};

use crate::{spec::Spec, Cpu, GeneratedAsset, GeneratedAssetKind, Os, PlatformDirectory};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use semver::Version;
use sha2::{Digest, Sha256};
use zip::{result::ZipError, write::FileOptions, ZipWriter};

mod templates {
    use crate::{pip::platform_target_tag, Cpu, Os};

    use super::PipPackage;

    pub(crate) fn dist_info_metadata(pkg: &PipPackage) -> String {
        let name = &pkg.package_name;
        let version = &pkg.package_version;
        format!(
            "Metadata-Version: 2.1
Name: {name}
Version: {version}
Home-page: https://TODO.com
Author: TODO
License: MIT License, Apache License, Version 2.0
Description-Content-Type: text/markdown

TODO readme"
        )
    }

    pub(crate) fn dist_info_wheel(platform: Option<(&Os, &Cpu)>) -> String {
        let name = env!("CARGO_PKG_NAME");
        let version = env!("CARGO_PKG_VERSION");
        let platform_tag = match platform {
            Some((os, cpu)) => platform_target_tag(os, cpu),
            None => "any".to_owned(),
        };
        let tag = format!("py3-none-{platform_tag}");
        format!(
            "Wheel-Version: 1.0
Generator: {name} {version}
Root-Is-Purelib: false
Tag: {tag}",
        )
    }
    pub(crate) fn dist_info_top_level_txt(pkg: &PipPackage) -> String {
        format!("{}\n", pkg.python_package_name)
    }

    pub(crate) fn dist_info_record(pkg: &PipPackage, record_path: &str) -> String {
        let mut record = String::new();
        for file in &pkg.written_files {
            record.push_str(format!("{},sha256={},{}\n", file.path, file.hash, file.size).as_str());
        }

        // RECORD one can be empty
        record.push_str(format!("{},,\n", record_path).as_str());

        record
    }
    pub(crate) fn base_init_py(pkg: &PipPackage, entrypoint: &str) -> String {
        let version = &pkg.package_version;
        format!(
            r#"
import os
import sqlite3

__version__ = "{version}"
__version_info__ = tuple(__version__.split("."))

def loadable_path():
  loadable_path = os.path.join(os.path.dirname(__file__), "{entrypoint}")
  return os.path.normpath(loadable_path)

def load(conn: sqlite3.Connection)  -> None:
  conn.load_extension(loadable_path())
"#,
        )
    }

    pub(crate) fn sqlite_utils_init_py(pkg: &PipPackage) -> String {
        let dep_library = pkg.python_package_name.clone();
        let version = pkg.package_version.clone();
        format!(
            r#"
from sqlite_utils import hookimpl
import {dep_library}

__version__ = "{version}"
__version_info__ = tuple(__version__.split("."))

@hookimpl
def prepare_connection(conn):
  conn.enable_load_extension(True)
  {dep_library}.load(conn)
  conn.enable_load_extension(False)
"#
        )
    }

    pub(crate) fn datasette_init_py(pkg: &PipPackage) -> String {
        let dep_library = pkg.python_package_name.clone();
        let version = pkg.package_version.clone();
        format!(
            r#"
from datasette import hookimpl
import {dep_library}

__version__ = "{version}"
__version_info__ = tuple(__version__.split("."))

@hookimpl
def prepare_connection(conn):
  conn.enable_load_extension(True)
  {dep_library}.load(conn)
  conn.enable_load_extension(False)
"#,
        )
    }
}

pub struct PipPackageFile {
    path: String,
    hash: String,
    size: usize,
}

impl PipPackageFile {
    fn new(path: &str, data: &[u8]) -> Self {
        let hash = URL_SAFE_NO_PAD.encode(Sha256::digest(data));
        Self {
            path: path.to_owned(),
            hash,
            size: data.len(),
        }
    }
}

fn semver_to_pip_version(v: &Version) -> String {
    match (
        (!v.pre.is_empty()).then(|| v.pre.clone()),
        (!v.build.is_empty()).then(|| v.build.clone()),
    ) {
        (None, None) => v.to_string(),
        // ???
        (None, Some(_build)) => v.to_string(),
        (Some(pre), None) => {
            let base = Version::new(v.major, v.minor, v.patch).to_string();
            let (a, b) = pre.split_once('.').unwrap();
            match a {
                "alpha" => format!("{base}a{b}"),
                "beta" => format!("{base}b{b}"),
                "rc" => format!("{base}rc{b}"),
                _ => todo!(),
            }
        }
        (Some(_pre), Some(_build)) => todo!(),
    }
    /*if v.pre.is_empty() && v.build.is_empty() {
        v.to_string()
    } else if v.build.is_empty() {
    }*/
}

pub fn platform_target_tag(os: &Os, cpu: &Cpu) -> String {
    match (os, cpu) {
        (Os::Macos, Cpu::X86_64) => "macosx_10_6_x86_64".to_owned(),
        (Os::Macos, Cpu::Aarch64) => "macosx_11_0_arm64".to_owned(),
        (Os::Linux, Cpu::X86_64) => {
            "manylinux_2_17_x86_64.manylinux2014_x86_64.manylinux1_x86_64".to_owned()
        }
        (Os::Linux, Cpu::Aarch64) => "manylinux_2_17_aarch64.manylinux2014_aarch64.whl".to_owned(),
        (Os::Windows, Cpu::X86_64) => "win_amd64".to_owned(),
        (Os::Windows, Cpu::Aarch64) => todo!(),
    }
}

pub struct PipPackage {
    pub zipfile: ZipWriter<Cursor<Vec<u8>>>,
    // as-is, with dashes, not python code safe
    pub package_name: String,
    // dashes replaced with underscores
    pub python_package_name: String,

    // not semver, but the special pip version string (ex 1.2a3)
    pub package_version: String,
    pub written_files: Vec<PipPackageFile>,
}

impl PipPackage {
    pub fn new<S: Into<String>>(package_name: S, package_version: &Version) -> Self {
        let buffer = Cursor::new(Vec::new());
        let zipfile = zip::ZipWriter::new(buffer);
        let package_name = package_name.into();
        Self {
            zipfile,
            package_name: package_name.clone(),
            python_package_name: package_name.replace('-', "_"),
            package_version: semver_to_pip_version(package_version),
            written_files: vec![],
        }
    }

    pub fn wheel_name(&self, platform: Option<(&Os, &Cpu)>) -> String {
        let name = &self.python_package_name;
        let version = &self.package_version;
        let python_tag = "py3";
        let abi_tag = "none";
        let platform_tag = match platform {
            Some((os, cpu)) => platform_target_tag(os, cpu),
            None => "any".to_owned(),
        };
        format!("{name}-{version}-{python_tag}-{abi_tag}-{platform_tag}.whl")
    }

    fn write_file(&mut self, path: &str, data: &[u8]) -> Result<(), ZipError> {
        let options = FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        self.zipfile.start_file(path, options)?;
        self.zipfile.write_all(data)?;
        self.written_files.push(PipPackageFile::new(path, data));
        Ok(())
    }

    pub fn write_library_file(&mut self, path: &str, data: &[u8]) -> Result<(), ZipError> {
        self.write_file(
            format!("{}/{}", self.python_package_name, path).as_str(),
            data,
        )
    }

    fn dist_info_file(&self, file: &str) -> String {
        format!(
            "{}-{}.dist-info/{}",
            self.python_package_name, self.package_version, file
        )
    }

    fn write_dist_info_metadata(&mut self) -> Result<(), ZipError> {
        self.write_file(
            self.dist_info_file("METADATA").as_str(),
            templates::dist_info_metadata(self).as_bytes(),
        )
    }

    fn write_dist_info_record(&mut self) -> Result<(), ZipError> {
        let record_path = self.dist_info_file("RECORD");
        self.write_file(
            &record_path,
            templates::dist_info_record(self, &record_path).as_bytes(),
        )
    }
    fn write_dist_info_top_level_txt(&mut self) -> Result<(), ZipError> {
        self.write_file(
            self.dist_info_file("top_level.txt").as_str(),
            templates::dist_info_top_level_txt(self).as_bytes(),
        )
    }
    fn write_dist_info_wheel(&mut self, platform: Option<(&Os, &Cpu)>) -> Result<(), ZipError> {
        self.write_file(
            self.dist_info_file("WHEEL").as_str(),
            templates::dist_info_wheel(platform).as_bytes(),
        )
    }

    pub fn end(mut self, platform: Option<(&Os, &Cpu)>) -> Result<Cursor<Vec<u8>>, ZipError> {
        self.write_dist_info_metadata()?;
        self.write_dist_info_wheel(platform)?;
        self.write_dist_info_top_level_txt()?;
        self.write_dist_info_record()?;
        self.zipfile.finish()
    }
}

use thiserror::Error;

#[derive(Error, Debug)]
pub enum PipBuildError {
    #[error("Zipfile error: {0}")]
    ZipError(#[from] ZipError),
    #[error("I/O error: {0}")]
    IOError(#[from] io::Error),
}

pub(crate) fn write_base_packages(
    pip_path: &Path,
    platform_dirs: &[PlatformDirectory],
    spec: &Spec,
) -> Result<Vec<GeneratedAsset>, PipBuildError> {
    let mut assets = vec![];
    for platform_dir in platform_dirs {
        let mut pkg = PipPackage::new(&spec.package.name, &spec.package.version);
        assert!(platform_dir.loadable_files.len() >= 1);
        let entrypoint = &platform_dir.loadable_files.get(0).expect("TODO").file_stem;
        pkg.write_library_file(
            "__init__.py",
            templates::base_init_py(&pkg, entrypoint).as_bytes(),
        )?;

        for f in &platform_dir.loadable_files {
            pkg.write_library_file(f.file.name.as_str(), &f.file.data)?;
        }
        let platform = Some((&platform_dir.os, &platform_dir.cpu));
        let wheel_name = pkg.wheel_name(platform);
        let result = pkg.end(platform)?.into_inner();
        let wheel_path = pip_path.join(wheel_name);
        assets.push(GeneratedAsset::from(
            GeneratedAssetKind::Pip((platform_dir.os.clone(), platform_dir.cpu.clone())),
            &wheel_path,
            &result,
        )?);
    }
    Ok(assets)
}

pub(crate) fn write_datasette(
    datasette_path: &Path,
    spec: &Spec,
) -> Result<GeneratedAsset, PipBuildError> {
    let datasette_package_name = format!("datasette-{}", spec.package.name);
    let mut pkg = PipPackage::new(datasette_package_name, &spec.package.version);
    pkg.write_library_file("__init__.py", templates::datasette_init_py(&pkg).as_bytes())?;

    let wheel_name = pkg.wheel_name(None);
    let result = pkg.end(None)?.into_inner();
    Ok(GeneratedAsset::from(
        GeneratedAssetKind::Datasette,
        &datasette_path.join(wheel_name),
        &result,
    )?)
}

pub(crate) fn write_sqlite_utils(
    sqlite_utils_path: &Path,
    spec: &Spec,
) -> Result<GeneratedAsset, PipBuildError> {
    let sqlite_utils_name = format!("sqlite-utils-{}", spec.package.name);
    let mut pkg = PipPackage::new(sqlite_utils_name, &spec.package.version);
    pkg.write_library_file(
        "__init__.py",
        templates::sqlite_utils_init_py(&pkg).as_bytes(),
    )?;

    let wheel_name = pkg.wheel_name(None);

    let result = pkg.end(None)?.into_inner();
    Ok(GeneratedAsset::from(
        GeneratedAssetKind::SqliteUtils,
        &sqlite_utils_path.join(wheel_name),
        &result,
    )?)
}
