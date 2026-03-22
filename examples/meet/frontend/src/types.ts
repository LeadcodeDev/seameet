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
  | { type: 'error'; code: number; message: string }
