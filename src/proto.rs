//! Generated protobuf types for the station protocol (build.rs, protox + prost)

pub mod google {
    pub mod protobuf {
        include!(concat!(env!("OUT_DIR"), "/google.protobuf.rs"));
    }
}

pub mod n_matrix {
    pub mod n_api {
        include!(concat!(env!("OUT_DIR"), "/n_matrix.n_api.rs"));
    }
}

pub mod nyt {
    include!(concat!(env!("OUT_DIR"), "/nyt.rs"));
}

pub mod n_alice_yt {
    include!(concat!(env!("OUT_DIR"), "/n_alice_yt.rs"));
}

pub mod n_alice {
    include!(concat!(env!("OUT_DIR"), "/n_alice.rs"));

    pub mod n_api {
        include!(concat!(env!("OUT_DIR"), "/n_alice.n_api.rs"));

        pub mod n_alice2_mode {
            include!(concat!(env!("OUT_DIR"), "/n_alice.n_api.n_alice2_mode.rs"));
        }

        pub mod n_files {
            include!(concat!(env!("OUT_DIR"), "/n_alice.n_api.n_files.rs"));
        }
    }

    pub mod n_alice_api {
        include!(concat!(env!("OUT_DIR"), "/n_alice.n_alice_api.rs"));
    }

    pub mod n_data {
        include!(concat!(env!("OUT_DIR"), "/n_alice.n_data.rs"));

        pub mod n_io_t {
            include!(concat!(env!("OUT_DIR"), "/n_alice.n_data.n_io_t.rs"));
        }

        pub mod n_music {
            include!(concat!(env!("OUT_DIR"), "/n_alice.n_data.n_music.rs"));
        }

        pub mod n_reminders {
            include!(concat!(env!("OUT_DIR"), "/n_alice.n_data.n_reminders.rs"));
        }
    }

    pub mod n_datum_api {
        include!(concat!(env!("OUT_DIR"), "/n_alice.n_datum_api.rs"));
    }

    pub mod n_quasar_auxiliary_config {
        include!(concat!(env!("OUT_DIR"), "/n_alice.n_quasar_auxiliary_config.rs"));
    }

    pub mod n_tv {
        include!(concat!(env!("OUT_DIR"), "/n_alice.n_tv.rs"));
    }
}
