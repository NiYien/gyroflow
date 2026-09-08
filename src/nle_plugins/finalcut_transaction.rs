// SPDX-License-Identifier: GPL-3.0-or-later
use std::io;
use std::path::{Path, PathBuf};

pub(super) struct InstallationLock(std::fs::File);

impl Drop for InstallationLock {
    fn drop(&mut self) {
        // Explicit unlock also releases locks inherited during another thread's fork.
        let _ = self.0.unlock();
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct Transaction {
    pub destination: PathBuf,
    pub companion: PathBuf,
    pub previous_destination: PathBuf,
    pub transaction_id: String,
    pub had_destination: bool,
    pub had_companion: bool,
    pub had_previous_app: bool,
    pub template: PathBuf,
    pub had_template: bool,
    pub committed: bool,
}

fn quote(path: impl AsRef<std::ffi::OsStr>) -> String {
    format!(
        "'{}'",
        path.as_ref().to_string_lossy().replace('\'', "'\\''")
    )
}

impl Transaction {
    pub fn acquire_lock(home: &Path) -> io::Result<InstallationLock> {
        let directory = home.join("Library/Caches/com.niyien.gyroflow");
        std::fs::create_dir_all(&directory)?;
        let path = directory.join("finalcut-install.lock");
        if path.is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Linked installer lock",
            ));
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        file.try_lock().map_err(|_| {
            io::Error::new(
                io::ErrorKind::WouldBlock,
                "Another NiYien FCP installation or recovery is running",
            )
        })?;
        // The OS releases this lock even if the installer process is interrupted.
        Ok(InstallationLock(file))
    }

    pub fn journal(&self) -> PathBuf {
        self.destination.with_file_name(".niyien-fcp-install.json")
    }

    pub fn backup(&self, app: &Path) -> PathBuf {
        app.with_file_name(format!(
            "{}.backup-{}",
            app.file_name().unwrap().to_string_lossy(),
            self.transaction_id
        ))
    }

    pub fn staging(&self) -> PathBuf {
        self.destination.with_file_name(format!(
            "{}.staging-{}",
            self.destination.file_name().unwrap().to_string_lossy(),
            self.transaction_id
        ))
    }

    pub fn template_backup(&self) -> PathBuf {
        self.destination.with_file_name(format!(
            ".niyien-fcp-template-backup-{}",
            self.transaction_id
        ))
    }

    fn template_staging(&self) -> PathBuf {
        self.template_backup().with_extension("staging")
    }

    pub fn validate(&self, parent: &Path, current_name: &str, legacy_name: &str) -> io::Result<()> {
        let current = parent.join(current_name);
        let legacy = parent.join(legacy_name);
        let pair_valid = (self.destination == current && self.companion == legacy)
            || (self.destination == legacy && self.companion == current);
        let previous = if self.had_destination {
            &self.destination
        } else if self.had_companion {
            &self.companion
        } else {
            &self.destination
        };
        if !pair_valid
            || uuid::Uuid::parse_str(&self.transaction_id).is_err()
            || self.transaction_id.contains('-')
            || self.had_previous_app != (self.had_destination || self.had_companion)
            || &self.previous_destination != previous
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid NiYien FCP transaction journal",
            ));
        }
        Ok(())
    }

    fn save_journal_command(&self, initial: bool) -> String {
        let temporary = self.journal().with_extension("json.XXXXXX");
        let json = serde_json::to_string(self).expect("transaction paths are JSON encodable");
        format!(
            "niyien_journal_tmp=$(/usr/bin/mktemp {temporary}); \
             trap '/bin/rm -f \"$niyien_journal_tmp\"' EXIT; \
             /usr/bin/printf '%s' {json} > \"$niyien_journal_tmp\"; \
             /bin/chmod 644 \"$niyien_journal_tmp\"; \
             /bin/{publish} \"$niyien_journal_tmp\" {journal}; ",
            temporary = quote(temporary),
            json = quote(json),
            publish = if initial { "ln" } else { "mv -f" },
            journal = quote(self.journal()),
        )
    }

    pub fn install_command(&self, source: &Path) -> String {
        let staging = self.staging();
        let destination_backup = self.backup(&self.destination);
        let companion_backup = self.backup(&self.companion);
        let template_copy = if self.had_template {
            format!(
                "/usr/bin/ditto {} {}; /bin/mv {} {}; ",
                quote(&self.template),
                quote(self.template_staging()),
                quote(self.template_staging()),
                quote(self.template_backup())
            )
        } else {
            String::new()
        };
        format!(
            "set -eu; test ! -e {journal}; test ! -L {journal}; \
             test ! -e {staging}; test ! -L {staging}; test ! -e {first}; test ! -L {first}; \
             test ! -e {second}; test ! -L {second}; test ! -e {template_backup}; test ! -L {template_backup}; \
             test ! -e {template_staging}; test ! -L {template_staging}; \
             test ! -L {destination}; test ! -L {companion}; \
             test {destination_test} -e {destination}; test {companion_test} -e {companion}; \
             {save}/usr/bin/ditto {source} {staging}; {template_copy}\
             if [ -e {destination} ]; then /bin/mv {destination} {first}; fi; \
             if [ -e {companion} ]; then /bin/mv {companion} {second}; fi; \
             /bin/mv {staging} {destination}",
            journal = quote(self.journal()),
            staging = quote(staging),
            first = quote(destination_backup),
            second = quote(companion_backup),
            source = quote(source),
            save = self.save_journal_command(true),
            destination = quote(&self.destination),
            companion = quote(&self.companion),
            template_backup = quote(self.template_backup()),
            template_staging = quote(self.template_staging()),
            destination_test = if self.had_destination { "" } else { "!" },
            companion_test = if self.had_companion { "" } else { "!" },
            template_copy = template_copy,
        )
    }

    pub fn rollback_command(&self) -> String {
        let mut command = "set -eu; ".to_owned();
        for (app, existed) in [
            (&self.destination, self.had_destination),
            (&self.companion, self.had_companion),
        ] {
            let backup = quote(self.backup(app));
            let target = quote(app);
            command.push_str(&format!(
                "if [ -e {backup} ]; then /bin/rm -rf {target}; /bin/mv {backup} {target}; "
            ));
            if !existed {
                command.push_str(&format!("else /bin/rm -rf {target}; "));
            }
            command.push_str("fi; ");
        }
        if self.had_template {
            command.push_str(&format!("if [ -d {backup} ]; then /bin/rm -rf {target}; /usr/bin/ditto {backup} {target}; fi; ",
                                      backup = quote(self.template_backup()), target = quote(&self.template)));
        } else {
            command.push_str(&format!("/bin/rm -rf {}; ", quote(&self.template)));
        }
        // Keep the journal until template and registration recovery also succeeds.
        command.push_str(&format!(
            "/bin/rm -rf {} {}",
            quote(self.staging()),
            quote(self.template_staging())
        ));
        command
    }

    pub fn commit_command(&self) -> String {
        let mut committed = self.clone();
        committed.committed = true;
        format!(
            "set -eu; {} /bin/rm -rf {} {} {} {} {}; /bin/rm -f {}",
            committed.save_journal_command(false),
            quote(self.backup(&self.destination)),
            quote(self.backup(&self.companion)),
            quote(self.staging()),
            quote(self.template_backup()),
            quote(self.template_staging()),
            quote(self.journal())
        )
    }

    pub fn finish_recovery_command(&self) -> String {
        format!(
            "set -eu; /bin/rm -rf {}; /bin/rm -f {}",
            quote(self.template_backup()),
            quote(self.journal())
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(parent: &Path, had_destination: bool, had_companion: bool) -> Transaction {
        let destination = parent.join("NiYien FCP.app");
        let companion = parent.join("GyroflowNiYien Final Cut.app");
        Transaction {
            previous_destination: if had_destination || !had_companion {
                destination.clone()
            } else {
                companion.clone()
            },
            destination,
            companion,
            transaction_id: uuid::Uuid::new_v4().simple().to_string(),
            had_destination,
            had_companion,
            had_previous_app: had_destination || had_companion,
            committed: false,
            template: parent.join("template"),
            had_template: false,
        }
    }

    fn shell(command: String) {
        assert!(
            std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg(command)
                .status()
                .unwrap()
                .success()
        );
    }

    #[test]
    fn rollback_before_backup_preserves_original_destination() {
        let root = tempfile::tempdir().unwrap();
        let tx = fixture(root.path(), true, true);
        std::fs::write(&tx.destination, b"current").unwrap();
        std::fs::write(&tx.companion, b"legacy").unwrap();
        shell(tx.rollback_command());
        assert_eq!(std::fs::read(&tx.destination).unwrap(), b"current");
        assert_eq!(std::fs::read(&tx.companion).unwrap(), b"legacy");
    }

    #[test]
    fn partial_migration_restores_both_original_names_and_retains_journal() {
        let root = tempfile::tempdir().unwrap();
        let tx = fixture(root.path(), true, true);
        std::fs::write(tx.backup(&tx.destination), b"old-current").unwrap();
        std::fs::write(tx.backup(&tx.companion), b"old-legacy").unwrap();
        std::fs::write(&tx.destination, b"new").unwrap();
        std::fs::write(tx.journal(), b"journal").unwrap();
        shell(tx.rollback_command());
        assert_eq!(std::fs::read(&tx.destination).unwrap(), b"old-current");
        assert_eq!(std::fs::read(&tx.companion).unwrap(), b"old-legacy");
        assert!(tx.journal().exists());
    }

    #[test]
    fn renamed_first_destination_is_removed_on_rollback() {
        let root = tempfile::tempdir().unwrap();
        let tx = fixture(root.path(), false, true);
        std::fs::write(tx.backup(&tx.companion), b"old-legacy").unwrap();
        std::fs::write(&tx.destination, b"new").unwrap();
        shell(tx.rollback_command());
        assert!(!tx.destination.exists());
        assert_eq!(std::fs::read(&tx.companion).unwrap(), b"old-legacy");
    }

    #[test]
    fn journal_rejects_outside_paths_and_invalid_transaction_id() {
        let root = tempfile::tempdir().unwrap();
        let mut tx = fixture(root.path(), false, true);
        tx.validate(
            root.path(),
            "NiYien FCP.app",
            "GyroflowNiYien Final Cut.app",
        )
        .unwrap();
        tx.companion = PathBuf::from("/unrelated.app");
        assert!(
            tx.validate(
                root.path(),
                "NiYien FCP.app",
                "GyroflowNiYien Final Cut.app"
            )
            .is_err()
        );
    }

    #[test]
    fn installer_lock_is_exclusive_and_released_on_drop() {
        let root = tempfile::tempdir().unwrap();
        let lock = Transaction::acquire_lock(root.path()).unwrap();
        assert!(Transaction::acquire_lock(root.path()).is_err());
        drop(lock);
        assert!(Transaction::acquire_lock(root.path()).is_ok());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn failed_copy_leaves_a_recoverable_journal_without_moving_the_old_app() {
        let root = tempfile::tempdir().unwrap();
        let tx = fixture(root.path(), false, true);
        std::fs::write(&tx.companion, b"old-app").unwrap();
        let status = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(tx.install_command(&root.path().join("missing-source.app")))
            .output()
            .unwrap()
            .status;
        assert!(!status.success());
        let journal: Transaction =
            serde_json::from_slice(&std::fs::read(tx.journal()).unwrap()).unwrap();
        assert_eq!(journal.transaction_id, tx.transaction_id);
        shell(tx.rollback_command());
        shell(tx.finish_recovery_command());
        assert_eq!(std::fs::read(&tx.companion).unwrap(), b"old-app");
        assert!(!tx.destination.exists());
        assert!(!tx.staging().exists());
    }

    #[test]
    fn interrupted_template_snapshot_is_never_restored_over_the_original() {
        let root = tempfile::tempdir().unwrap();
        let mut tx = fixture(root.path(), false, false);
        tx.had_template = true;
        std::fs::create_dir(&tx.template).unwrap();
        std::fs::create_dir(tx.template_staging()).unwrap();
        std::fs::write(tx.template.join("payload"), b"complete-original").unwrap();
        std::fs::write(tx.template_staging().join("payload"), b"partial").unwrap();
        shell(tx.rollback_command());
        assert_eq!(
            std::fs::read(tx.template.join("payload")).unwrap(),
            b"complete-original"
        );
        assert!(!tx.template_staging().exists());
    }

    #[test]
    fn existing_transaction_is_never_overwritten() {
        let root = tempfile::tempdir().unwrap();
        let tx = fixture(root.path(), false, true);
        std::fs::write(&tx.companion, b"old-app").unwrap();
        std::fs::write(tx.journal(), b"another-transaction").unwrap();
        let status = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(tx.install_command(&root.path().join("source.app")))
            .status()
            .unwrap();
        assert!(!status.success());
        assert_eq!(std::fs::read(tx.journal()).unwrap(), b"another-transaction");
        assert_eq!(std::fs::read(&tx.companion).unwrap(), b"old-app");
        assert!(!tx.staging().exists());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn complete_rename_rolls_back_the_original_template_and_handles_quoted_paths() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("user's files; no shell expansion");
        std::fs::create_dir(&root).unwrap();
        let mut tx = fixture(&root, false, true);
        tx.had_template = true;
        let source = root.join("source.app");
        for path in [&source, &tx.companion, &tx.template] {
            std::fs::create_dir(path).unwrap();
        }
        std::fs::write(source.join("payload"), b"new-app").unwrap();
        std::fs::write(tx.companion.join("payload"), b"old-app").unwrap();
        std::fs::write(tx.template.join("payload"), b"old-template").unwrap();
        shell(tx.install_command(&source));
        assert!(!tx.companion.exists());
        assert_eq!(
            std::fs::read(tx.destination.join("payload")).unwrap(),
            b"new-app"
        );
        assert!(tx.journal().is_file());
        std::fs::write(tx.template.join("payload"), b"new-template").unwrap();
        shell(tx.rollback_command());
        assert!(!tx.destination.exists());
        assert_eq!(
            std::fs::read(tx.companion.join("payload")).unwrap(),
            b"old-app"
        );
        assert_eq!(
            std::fs::read(tx.template.join("payload")).unwrap(),
            b"old-template"
        );
        assert!(tx.journal().exists());
        shell(tx.finish_recovery_command());
        assert!(!tx.journal().exists());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn explicit_commit_keeps_new_app_and_discards_only_transaction_backups() {
        let root = tempfile::tempdir().unwrap();
        let tx = fixture(root.path(), false, true);
        let source = root.path().join("source.app");
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(&tx.companion).unwrap();
        std::fs::write(source.join("payload"), b"new-app").unwrap();
        shell(tx.install_command(&source));
        shell(tx.commit_command());
        assert!(tx.destination.join("payload").is_file());
        assert!(source.join("payload").is_file());
        assert!(!tx.backup(&tx.companion).exists());
        assert!(!tx.journal().exists());
    }
}
