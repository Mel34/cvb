use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn create_rcfile() -> Result<PathBuf, String> {
    let parent_init =
        env::var_os("CVB_INIT_FILE").ok_or_else(|| "CVB: CVB_INIT_FILE is not set".to_string())?;

    let parent_history = env::var_os("CVB_PARENT_HISTORY_FILE")
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

    let content = format!(
        "set +o history\n\
         \n\
         if [[ -f {bashrc} ]]; then\n\
             source {bashrc}\n\
         fi\n\
         \n\
         source {parent_init}\n\
         {control_script}\n\
         \n\
         history -c\n\
         history -r {parent_history}\n\
         \n\
         set -o history\n",
        bashrc = bash_quote(&bashrc),
        parent_init = bash_quote(Path::new(&parent_init)),
        control_script = control_script,
        parent_history = bash_quote(Path::new(&parent_history)),
    );

    fs::write(&rcfile, content).map_err(|error| {
        format!(
            "CVB: unable to create Bash rcfile {}: {error}",
            rcfile.display()
        )
    })?;

    Ok(rcfile)
}

fn control_script() -> Result<String, String> {
    Ok(include_str!("../data/cvb-control.bash").to_string())
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
