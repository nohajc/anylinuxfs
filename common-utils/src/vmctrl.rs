use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum Request {
    Quit,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum Response {
    Ack,
}
