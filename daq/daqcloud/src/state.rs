use daqcore::storage::Registry;

#[derive(Clone)]
pub struct AppState {
    #[allow(dead_code)]
    pub registry: Registry,
}
