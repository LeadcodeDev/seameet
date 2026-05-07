import React, { useMemo } from 'react'
import { useCall } from '@/context/CallContext'
import { VideoTile } from '@/components/VideoTile'
import { AvatarTile } from '@/components/AvatarTile'
import { classifyRoom, pickVisibleVideoSet } from '@/lib/roomMode'

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
    activeSpeakerId,
    recentSpeakers,
    participantId,
    verificationStatus,
  } = useCall()

  const peers = useMemo(() => Array.from(remotePeers.values()), [remotePeers])
  const screenShareCount = (localScreenStream ? 1 : 0) + peers.filter(p => p.screenStream).length
  const totalTiles = 1 + peers.length + screenShareCount

  const mode = classifyRoom(totalTiles)
  const visibleVideoIds = useMemo(
    () =>
      pickVisibleVideoSet({
        peerIds: peers.map(p => p.id),
        localId: participantId,
        activeSpeakerId,
        recentSpeakers,
        mode,
      }),
    [peers, participantId, activeSpeakerId, recentSpeakers, mode],
  )

  // Use spotlight layout when 5+ tiles and an active speaker is detected.
  // Disabled in large/webinar modes — those have their own layout.
  const useSpotlight = mode === 'meeting' && totalTiles >= 5 && activeSpeakerId !== null

  if (useSpotlight) {
    // Find the active speaker peer (or local if it's us)
    const isLocalSpeaker = activeSpeakerId === participantId
    const speakerPeer = !isLocalSpeaker ? peers.find(p => p.id === activeSpeakerId) : null
    const filmstripPeers = peers.filter(p => p.id !== activeSpeakerId)

    return (
      <div data-testid="video-grid" data-room-mode={mode} className="flex flex-col h-full gap-2">
        {/* Spotlight: large tile for active speaker */}
        <div className="flex-1 min-h-0">
          {isLocalSpeaker ? (
            <VideoTile
              stream={localStream}
              name={displayName}
              isLocal
              audioEnabled={audioEnabled}
              videoEnabled={videoEnabled}
              e2eeActive={e2eeEnabled}
              isActiveSpeaker
            />
          ) : speakerPeer ? (
            <VideoTile
              stream={speakerPeer.stream}
              name={speakerPeer.displayName}
              isLocal={false}
              audioEnabled={!speakerPeer.audioMuted}
              videoEnabled={!e2eeEnabled && speakerPeer.e2ee ? false : !speakerPeer.videoMuted}
              e2eeActive={e2eeEnabled && e2eePeerStates.get(speakerPeer.id)?.ready}
              isActiveSpeaker
              verification={verificationStatus(speakerPeer.id)}
            />
          ) : null}
        </div>

        {/* Filmstrip: horizontal row of other participants */}
        <div className="flex gap-2 h-32 shrink-0 overflow-x-auto">
          {/* Local (if not speaker) */}
          {!isLocalSpeaker && (
            <div className="w-48 shrink-0 h-full">
              <VideoTile
                stream={localStream}
                name={displayName}
                isLocal
                audioEnabled={audioEnabled}
                videoEnabled={videoEnabled}
                e2eeActive={e2eeEnabled}
              />
            </div>
          )}

          {/* Local screen share */}
          {localScreenStream && (
            <div className="w-48 shrink-0 h-full">
              <VideoTile
                stream={localScreenStream}
                name={displayName}
                isLocal
                isScreenShare
                audioEnabled
                videoEnabled
              />
            </div>
          )}

          {/* Other remote peers */}
          {filmstripPeers.map((peer) => (
            <React.Fragment key={peer.id}>
              <div className="w-48 shrink-0 h-full">
                <VideoTile
                  stream={peer.stream}
                  name={peer.displayName}
                  isLocal={false}
                  audioEnabled={!peer.audioMuted}
                  videoEnabled={!e2eeEnabled && peer.e2ee ? false : !peer.videoMuted}
                  e2eeActive={e2eeEnabled && e2eePeerStates.get(peer.id)?.ready}
                  verification={verificationStatus(peer.id)}
                />
              </div>
              {peer.screenStream && (
                <div className="w-48 shrink-0 h-full">
                  <VideoTile
                    stream={peer.screenStream}
                    name={peer.displayName}
                    isLocal={false}
                    isScreenShare
                    audioEnabled
                    videoEnabled
                  />
                </div>
              )}
            </React.Fragment>
          ))}
        </div>
      </div>
    )
  }

  if (mode === 'meeting') {
    // Default grid layout
    const gridCols = (() => {
      if (totalTiles === 1) return 'grid-cols-1'
      if (totalTiles <= 2) return 'grid-cols-2'
      if (totalTiles <= 4) return 'grid-cols-2'
      return 'grid-cols-[repeat(auto-fit,minmax(300px,1fr))]'
    })()

    return (
      <div data-testid="video-grid" data-room-mode={mode} className={`grid ${gridCols} gap-2 h-full auto-rows-fr`}>
        {/* Local video */}
        <VideoTile
          stream={localStream}
          name={displayName}
          isLocal
          audioEnabled={audioEnabled}
          videoEnabled={videoEnabled}
          e2eeActive={e2eeEnabled}
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
              videoEnabled={!e2eeEnabled && peer.e2ee ? false : !peer.videoMuted}
              e2eeActive={e2eeEnabled && e2eePeerStates.get(peer.id)?.ready}
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

  // Large / webinar modes: split peers into video set vs avatar set.
  const videoPeers = peers.filter(p => visibleVideoIds.has(p.id))
  const avatarPeers = peers.filter(p => !visibleVideoIds.has(p.id))

  return (
    <div data-testid="video-grid" data-room-mode={mode} className="flex flex-col h-full gap-2">
      {/* Top region: video tiles for the speakers we keep + local + screen shares */}
      <div className="grid gap-2 grid-cols-[repeat(auto-fit,minmax(220px,1fr))] auto-rows-[180px] shrink-0">
        <VideoTile
          stream={localStream}
          name={displayName}
          isLocal
          audioEnabled={audioEnabled}
          videoEnabled={videoEnabled}
          e2eeActive={e2eeEnabled}
        />
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
        {videoPeers.map(peer => (
          <React.Fragment key={peer.id}>
            <VideoTile
              stream={peer.stream}
              name={peer.displayName}
              isLocal={false}
              audioEnabled={!peer.audioMuted}
              videoEnabled={!e2eeEnabled && peer.e2ee ? false : !peer.videoMuted}
              e2eeActive={e2eeEnabled && e2eePeerStates.get(peer.id)?.ready}
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

      {/* Bottom region: dense avatar grid for everyone else */}
      {avatarPeers.length > 0 && (
        <div
          data-testid="avatar-grid"
          className="flex-1 min-h-0 overflow-y-auto grid gap-1 grid-cols-[repeat(auto-fill,minmax(80px,1fr))] auto-rows-[80px] content-start"
        >
          {avatarPeers.map(peer => (
            <AvatarTile
              key={peer.id}
              name={peer.displayName}
              audioEnabled={!peer.audioMuted}
              isActiveSpeaker={peer.id === activeSpeakerId}
            />
          ))}
        </div>
      )}
    </div>
  )
}
