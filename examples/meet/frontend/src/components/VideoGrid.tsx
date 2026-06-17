import React, { useMemo } from 'react'
import { useCall } from '@/context/CallContext'
import { VideoTile } from '@/components/VideoTile'
import { VIDEO_GRID_BUDGET, pickVisibleVideoSet } from '@/lib/roomMode'

export function VideoGrid() {
  const {
    localStream,
    remotePeers,
    displayName,
    audioEnabled,
    videoEnabled,
    localScreenStream,
    e2eeEnabled,
    e2eePeerStates,
    e2eeNotReady,
    activeSpeakerId,
    recentSpeakers,
    participantId,
    verificationStatus,
  } = useCall()

  const peers = useMemo(() => Array.from(remotePeers.values()), [remotePeers])
  const screenShareCount = (localScreenStream ? 1 : 0) + peers.filter(p => p.screenStream).length
  const totalTiles = 1 + peers.length + screenShareCount

  const allVideo = totalTiles <= VIDEO_GRID_BUDGET
  const visibleVideoIds = useMemo(
    () =>
      allVideo
        ? new Set(peers.map(p => p.id))
        : pickVisibleVideoSet({
            peerIds: peers.map(p => p.id),
            localId: participantId,
            activeSpeakerId,
            recentSpeakers,
            budget: VIDEO_GRID_BUDGET,
          }),
    [allVideo, peers, participantId, activeSpeakerId, recentSpeakers],
  )

  return (
    <div
      data-testid="video-grid"
      className="grid gap-2 h-full w-full grid-cols-[repeat(auto-fit,minmax(220px,1fr))] auto-rows-fr"
    >
      {/* Local video */}
      <VideoTile
        stream={localStream}
        name={displayName}
        isLocal
        audioEnabled={audioEnabled}
        videoEnabled={videoEnabled}
        e2eeActive={e2eeEnabled}
        e2eeNotReady={e2eeEnabled && e2eeNotReady.local}
        isActiveSpeaker={activeSpeakerId === participantId}
      />

      {/* Local screen share */}
      {localScreenStream && (
        <VideoTile
          stream={localScreenStream}
          name={displayName}
          isLocal
          isScreenShare
          audioEnabled
          videoEnabled
        />
      )}

      {/* Remote peers */}
      {peers.map((peer) => (
        <React.Fragment key={peer.id}>
          <VideoTile
            stream={peer.stream}
            name={peer.displayName}
            isLocal={false}
            audioEnabled={!peer.audioMuted}
            videoEnabled={visibleVideoIds.has(peer.id) ? (!e2eeEnabled && peer.e2ee ? false : !peer.videoMuted) : false}
            e2eeActive={e2eeEnabled && e2eePeerStates.get(peer.id)?.ready}
            e2eeNotReady={e2eeEnabled && e2eeNotReady.peers.has(peer.id)}
            isActiveSpeaker={peer.id === activeSpeakerId}
            verification={verificationStatus(peer.id)}
          />
          {peer.screenStream && (
            <VideoTile
              stream={peer.screenStream}
              name={peer.displayName}
              isLocal={false}
              isScreenShare
              audioEnabled
              videoEnabled
            />
          )}
        </React.Fragment>
      ))}
    </div>
  )
}
