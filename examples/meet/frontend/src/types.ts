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
      }>;
    }
  | { type: 'error'; code: number; message: string }
