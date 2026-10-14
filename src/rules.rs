//! Rules that decide what counts as junk and which bucket it lands in.

use serde::Serialize;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Bucket {
    /// Regenerable: safe to remove with one tap.
    Safe,
    /// Needs the owner to look before anything is removed.
    NeedsYou,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    NodeModules,
    RustTarget,
    PythonVenv,
    PyCache,
    GradleBuild,
    NextBuild,
    PackageCache,
    AndroidAvd,
    OldInstaller,
    LargeFile,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::NodeModules => "node_modules",
            Kind::RustTarget => "Rust target/",
            Kind::PythonVenv => "Python virtualenv",
            Kind::PyCache => "__pycache__",
            Kind::GradleBuild => "Gradle build output",
            Kind::NextBuild => "Next.js .next/",
            Kind::PackageCache => "Package cache",
            Kind::AndroidAvd => "Android emulator image",
            Kind::OldInstaller => "Old installer",
            Kind::LargeFile => "Large file",
        }
    }

    /// How the item comes back after it is removed.
    pub fn restore_hint(self) -> &'static str {
        match self {
            Kind::NodeModules => "npm / pnpm / yarn install",
            Kind::RustTarget => "cargo build",
            Kind::PythonVenv => "python -m venv + pip install -r requirements.txt",
            Kind::PyCache => "regenerated automatically by Python",
            Kind::GradleBuild => "./gradlew build",
            Kind::NextBuild => "next build / next dev",
            Kind::PackageCache => "re-downloaded on next install",
            Kind::AndroidAvd => "recreate in Android Studio (settings and data are lost)",
            Kind::OldInstaller => "download it again from the vendor",
            Kind::LargeFile => "not restorable, check before deleting",
        }
    }
}

/// A project build artifact found inside a project folder.
/// Returns the kind when `dir` is a known artifact folder for the project in its parent.
pub fn classify_project_dir(dir: &Path) -> Option<Kind> {
    let name = dir.file_name()?.to_str()?;
    let parent = dir.parent()?;
    let has = |f: &str| parent.join(f).exists();
    match name {
        "node_modules" if has("package.json") => Some(Kind::NodeModules),
        "target" if has("Cargo.toml") => Some(Kind::RustTarget),
        ".venv" | "venv" | "env" if dir.join("pyvenv.cfg").exists() => Some(Kind::PythonVenv),
        "__pycache__" => Some(Kind::PyCache),
        "build" if has("build.gradle") || has("build.gradle.kts") => Some(Kind::GradleBuild),
        ".next" if has("package.json") => Some(Kind::NextBuild),
        _ => None,
    }
}

/// Installer and archive extensions that usually sit forgotten in Downloads.
pub fn is_installer(path: &Path) -> bool {
    const EXT: [&str; 10] = ["dmg", "pkg", "exe", "msi", "iso", "deb", "rpm", "appimage", "zip", "tar.gz"];
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    EXT.iter().any(|e| name.ends_with(&format!(".{e}")))
}

/// Folders never worth walking into while looking for project junk.
pub fn skip_walk(name: &str) -> bool {
    matches!(
        name,
        ".git" | ".hg" | ".svn" | "Library" | "AppData" | ".Trash" | "proc" | "sys" | "dev"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn node_modules_needs_package_json() {
        let tmp = tempfile::tempdir().unwrap();
        let nm = tmp.path().join("node_modules");
        fs::create_dir(&nm).unwrap();
        assert_eq!(classify_project_dir(&nm), None);
        fs::write(tmp.path().join("package.json"), "{}").unwrap();
        assert_eq!(classify_project_dir(&nm), Some(Kind::NodeModules));
    }

    #[test]
    fn target_needs_cargo_toml() {
        let tmp = tempfile::tempdir().unwrap();
        let t = tmp.path().join("target");
        fs::create_dir(&t).unwrap();
        assert_eq!(classify_project_dir(&t), None);
        fs::write(tmp.path().join("Cargo.toml"), "").unwrap();
        assert_eq!(classify_project_dir(&t), Some(Kind::RustTarget));
    }

    #[test]
    fn venv_needs_pyvenv_cfg() {
        let tmp = tempfile::tempdir().unwrap();
        let v = tmp.path().join(".venv");
        fs::create_dir(&v).unwrap();
        assert_eq!(classify_project_dir(&v), None);
        fs::write(v.join("pyvenv.cfg"), "").unwrap();
        assert_eq!(classify_project_dir(&v), Some(Kind::PythonVenv));
    }

    #[test]
    fn installers() {
        assert!(is_installer(Path::new("/d/Setup.EXE")));
        assert!(is_installer(Path::new("/d/app.tar.gz")));
        assert!(!is_installer(Path::new("/d/notes.txt")));
    }
}
