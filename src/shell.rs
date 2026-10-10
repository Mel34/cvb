use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn create_rcfile() -> Result<PathBuf, String> {
    verify_cvb_bash()?;
    let parent_init =
        env::var_os("CVB_INIT_FILE").ok_or_else(|| "CVB: CVB_INIT_FILE is not set".to_string())?;

    env::var_os("CVB_PARENT_HISTORY_FILE")
        .ok_or_else(|| "CVB: CVB_PARENT_HISTORY_FILE is not set".to_string())?;

    env::var_os("CVB_HISTORY_FILE")
        .ok_or_else(|| "CVB: CVB_HISTORY_FILE is not set".to_string())?;

    let runtime_dir = env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .ok_or_else(|| "CVB: XDG_RUNTIME_DIR is not set".to_string())?;

    let cvb_dir = runtime_dir.join("cvb");

    fs::create_dir_all(&cvb_dir).map_err(|error| {
        format!(
            "CVB: unable to create runtime directory {}: {error}",
            cvb_dir.display()
        )
    })?;

    let rcfile = cvb_dir.join(format!("rc-{}-{}", std::process::id(), timestamp_suffix()));

    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "CVB: HOME is not set".to_string())?;

    let bashrc = home.join(".bashrc");

    let control_script = control_script()?;
    let patch_function = patch_function();

    let content = format!(
        "set +o history\n\
         \n\
         if [[ -f {bashrc} ]]; then\n\
             source {bashrc}\n\
         fi\n\
         source {parent_init}\n\
         {control_script}\n\
         {patch_function}\n\
         \n\
         cvb_control_history_start\n",
        bashrc = bash_quote(&bashrc),
        parent_init = bash_quote(Path::new(&parent_init)),
        control_script = control_script,
        patch_function = patch_function,
    );

    fs::write(&rcfile, content).map_err(|error| {
        format!(
            "CVB: unable to create Bash rcfile {}: {error}",
            rcfile.display()
        )
    })?;

    Ok(rcfile)
}

include!(concat!(env!("OUT_DIR"), "/cvb_script_hashes.rs"));


fn verify_cvb_bash() -> Result<(), String> {
    use sha2::{Digest, Sha256};

    let loaded = env::var("CVB_BASH_SHA256")
        .map_err(|_| "CVB: CVB_BASH_SHA256 is not set; source cvb.bash again".to_string())?;

    if loaded != CVB_BASH_SHA256 {
        let source = env::var_os("CVB_BASH_SOURCE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/usr/share/cvb/cvb.bash"));

        return Err(format!(
            "CVB: the parent shell has an older cvb.bash loaded\n  expected: {}\n  loaded:   {}\nRefresh it in this shell by running:\n  source {}\nAlternatively, start a new shell.",
            CVB_BASH_SHA256,
            loaded,
            bash_quote(&source),
        ));
    }

    let path = env::var_os("CVB_BASH_SOURCE")
        .map(PathBuf::from)
        .ok_or_else(|| "CVB: CVB_BASH_SOURCE is not set; source cvb.bash again".to_string())?;

    let contents = fs::read(&path).map_err(|error| {
        format!(
            "CVB: unable to read {} for fingerprint verification: {error}",
            path.display()
        )
    })?;

    let actual: String = Sha256::digest(&contents)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();

    if actual != CVB_BASH_SHA256 {
        return Err(format!(
            "CVB: on-disk cvb.bash fingerprint mismatch at {}\n  expected: {}\n  actual:   {}\nRebuild and reinstall CVB so the binary and installed scripts match.\nThen refresh the parent shell with:\n  source /usr/share/cvb/cvb.bash",
            path.display(),
            CVB_BASH_SHA256,
            actual,
        ));
    }

    Ok(())
}

fn control_script() -> Result<String, String> {
    use sha2::{Digest, Sha256};

    let installed = PathBuf::from("/usr/share/cvb/cvb-control.bash");
    let development = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("data/cvb-control.bash");

    let path = if installed.is_file() {
        installed
    } else if development.is_file() {
        development
    } else {
        return Err(
            "CVB: unable to verify cvb-control.bash: script file not found".to_string()
        );
    };

    let contents = fs::read(&path).map_err(|error| {
        format!(
            "CVB: unable to read {} for fingerprint verification: {error}",
            path.display()
        )
    })?;

    let actual: String = Sha256::digest(&contents)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();

    if actual != CVB_CONTROL_BASH_SHA256 {
        return Err(format!(
            "CVB: cvb-control.bash fingerprint mismatch at {}\n  expected: {}\n  actual:   {}\nRebuild and reinstall CVB.",
            path.display(),
            CVB_CONTROL_BASH_SHA256,
            actual
        ));
    }

    Ok(include_str!("../data/cvb-control.bash").to_string())
}

fn patch_function() -> String {
    r#"
patch() {
    local arg
    local dry_run=0
    local verbose=0

    for arg in "$@"; do
        [[ "$arg" == "--dry-run" ]] && dry_run=1
        [[ "$arg" == "--verbose" ]] && verbose=1
    done

    if (( dry_run && verbose == 0 )); then
        command patch --verbose "$@"
    else
        command patch "$@"
    fi
}
"#.to_string()
}

fn bash_quote(path: &Path) -> String {
    let value = path.to_string_lossy();
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn timestamp_suffix() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.subsec_nanos())
        .unwrap_or(0)
}
