use std::env;
use std::fs;
use std::path::Path;

pub fn read_init_file() -> Result<Option<String>, String> {
    let Some(path) = env::var_os("CVB_INIT_FILE") else {
        return Ok(None);
    };

    let path = Path::new(&path);

    if !path.is_file() {
        return Err(format!(
            "CVB: shell state file does not exist: {}",
            path.display()
        ));
    }

    fs::read_to_string(path).map(Some).map_err(|error| {
        format!(
            "CVB: unable to read shell state file {}: {error}",
            path.display()
        )
    })
}
