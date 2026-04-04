#![no_std]

//! Subsumption Behavior Engine — Phase G1.
//!
//! Brooks-style layered behavior architecture with 4 priority levels:
//! - L0: emergency-stop  (IMU fall/spin detection, always active)
//! - L1: avoid-obstacle  (local MLP inference, gated by no-ml)
//! - L2: remote-vla      (TCP ↔ external VLA server)
//! - L3: explore          (default wander forward)
//!
//! The arbiter iterates L0→L3; the first layer that returns a valid
//! `MotorOutput` wins and is published to the RT motor task.

pub mod types;
pub mod layers;
pub mod arbiter;
pub mod remote;
pub mod brain_protocol;
pub mod offline;
pub mod safety;
pub mod balance;
pub mod sensor_bus;

// Re-export public API
pub use types::*;
pub use arbiter::{
    arbitrate, layer_set_enabled, layer_is_enabled, last_winner,
    layer_statuses, LAYER_NAMES,
};
pub use remote::{
    encode_observation, decode_action_packet, decode_goal_packet,
    remote_configure, remote_set_enabled, remote_is_enabled,
    remote_server_ip, remote_server_port, remote_info, RemoteInfo,
    current_goal, set_current_goal, last_action, set_last_action,
    remote_set_connected, remote_set_socket, remote_socket,
    remote_inc_sent, remote_inc_recv,
    OBS_TOTAL, ACTION_PACKET_SIZE, GOAL_PACKET_SIZE,
    OBS_MAGIC, ACT_MAGIC, GOAL_MAGIC,
};
pub use brain_protocol::{
    build_packet, parse_packet, crc8,
    encode_sensor_packet, encode_status_packet, decode_actuator_cmd,
    encode_camera_header,
    decode_mode_cmd, decode_waypoint_cmd, decode_config_cmd,
    ActuatorCmd as BrainActuatorCmd,
    ModeCmd, WaypointCmd, ConfigCmd,
    PKT_SENSOR, PKT_CAMERA, PKT_STATUS, PKT_OTA_ACK,
    PKT_ACTUATOR, PKT_MODE, PKT_WAYPOINT, PKT_CONFIG,
    PKT_OTA_BEGIN, PKT_OTA_CHUNK, PKT_OTA_END, PKT_ESTOP,
    ESTOP_REASON_OPERATOR, ESTOP_REASON_SAFETY, ESTOP_REASON_GEOFENCE,
    OTA_ACK_OK, OTA_ACK_ERROR,
    ROBOT_WHEELED, ROBOT_DRONE, ROBOT_HUMANOID, ROBOT_ACKERMANN,
    FLAG_EMERGENCY, FLAG_ALERT, ACT_DIFF_DRIVE,
    CAMERA_HDR_SIZE, CAMERA_FMT_GRAY8, CAMERA_FMT_JPEG,
    SENSOR_PAYLOAD_SIZE, STATUS_PAYLOAD_SIZE,
    MODE_PAYLOAD_SIZE, WAYPOINT_PAYLOAD_SIZE, CONFIG_PAYLOAD_SIZE,
    SENSOR_FRAME_SIZE, STATUS_FRAME_SIZE, FRAME_OVERHEAD,
    // Config keys + values
    CFG_KEY_LED, CFG_KEY_POWER, CFG_KEY_CAMERA, CFG_KEY_WIFI,
    CFG_KEY_LIDAR_HZ, CFG_KEY_BUZZER, CFG_KEY_SIREN,
    CFG_KEY_SPOTLIGHT, CFG_KEY_LASER,
    CFG_KEY_SERVO_PAN, CFG_KEY_SERVO_TILT, CFG_KEY_SPEAKER,
    BUZZER_OFF, BUZZER_BEEP, BUZZER_SIREN,
    CAMERA_PWR_OFF, CAMERA_PWR_ON,
};
