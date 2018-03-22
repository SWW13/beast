use std::path::Path;
use std::collections::BTreeMap;
use std::io::Read;
use std::fs::File;
use melon::typedef::*;
use toml;

#[derive(Deserialize, Debug)]
pub struct Program {
    /// The version of the melon library used by the target
    pub target: String,
    pub system_id: String,
    pub mem_pages: Option<u8>,
}

#[derive(Deserialize, Debug)]
pub struct Compilation {
    pub entry_point: Option<String>,
    pub absolute_module_paths: Option<bool>,
}

#[derive(Deserialize, Debug)]
pub struct Config {
    program: Program,
    compilation: Option<Compilation>,
    signals: Option<BTreeMap<String, u16>>,
}

impl Config {
    fn from_file<P: AsRef<Path>>(path: P) -> Result<Config> {
        let mut file = File::open(path)?;

        let mut buf = String::new();
        file.read_to_string(&mut buf)?;

        let config = toml::from_str(&buf)?;

        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_config() {
        const FILE_NAME: &str = "Beast.toml";

        let config = Config::from_file(FILE_NAME).unwrap();

        println!("{:#?}", config);
    }
}