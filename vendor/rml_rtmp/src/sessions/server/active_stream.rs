use super::PublishMode;

pub enum StreamState {
    Created,

    Publishing {
        stream_key: String,
        #[allow(dead_code)] // restream vendor patch: upstream-unused field, quiet build logs
        mode: PublishMode,
    },

    Playing {
        stream_key: String,
    },

    Completed,
}

pub struct ActiveStream {
    pub current_state: StreamState,
}
