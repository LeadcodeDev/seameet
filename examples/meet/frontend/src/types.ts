export type SignalingMessage =
  | { type: 'join'; participant: string; room_id: string; display_name?: string }
  | { type: 'leave'; participant: string; room_id: string }
  | { type: 'ready'; room_id: string; initiator: boolean; peers: string[]; display_names?: Record<string, string> }
  | { type: 'offer'; from: string; to: string | null; room_id: string; sdp: string }
  | { type: 'answer'; from: string; to: string; room_id: string; sdp: string }
  | { type: 'ice_candidate'; from: string; to: string; room_id: string; candidate: string; sdp_mid: string | null; sdp_mline_index: number | null }
  | { type: 'peer_joined'; participant: string; room_id: string; display_name?: string }
  | { type: 'peer_left'; participant: string; room_id: string }
  | { type: 'screen_share_started'; from: string; room_id: string; track_id: number }
  | { type: 'screen_share_stopped'; from: string; room_id: string; track_id: number }
  | { type: 'mute_audio'; from: string; room_id: string }
  | { type: 'unmute_audio'; from: string; room_id: string }
  | { type: 'mute_video'; from: string; room_id: string }
  | { type: 'unmute_video'; from: string; room_id: string }
  | { type: 'video_config_changed'; from: string; room_id: string; width: number; height: number; fps: number }
  | { type: 'request_renegotiation'; room_id: string; needed_slots: number }
  | {
      type: 'room_status';
      room_id: string;
      participants: Array<{
        id: string;
        display_name?: string;
        audio_muted: boolean;
        video_muted: boolean;
        screen_sharing: boolean;
        e2ee?: boolean;
      }>;
    }
  | { type: 'e2ee_public_key'; from: string; room_id: string; public_key: string }
  | { type: 'e2ee_sender_key'; from: string; to: string; room_id: string; encrypted_key: string; key_id: number }
  | { type: 'e2ee_key_rotation'; from: string; room_id: string; key_id: number }
  | { type: 'chat_message'; from: string; room_id: string; display_name?: string; content: string; timestamp: number; encrypted?: boolean; key_id?: number }
  | { type: 'active_speaker'; room_id: string; speaker: string; level: number }
  | { type: 'bwe_update'; max_bitrate_bps: number }
  | { type: 'error'; code: number; message: string }
