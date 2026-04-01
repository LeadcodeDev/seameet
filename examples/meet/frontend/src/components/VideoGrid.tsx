import React, { useMemo } from 'react'
import { useCall } from '@/context/CallContext'
import { VideoTile } from '@/components/VideoTile'

export function VideoGrid() {
  const { localStream, remotePeers, displayName, audioEnabled, videoEnabled, localScreenStream, e2eeEnabled, e2eePeerStates, activeSpeakerId, participantId } = useCall()

  const peers = useMemo(() => Array.from(remotePeers.values()), [remotePeers])
  const screenShareCount = (localScreenStream ? 1 : 0) + peers.filter(p => p.screenStream).length
  const totalTiles = 1 + peers.length + screenShareCount

  // Use spotlight layout when 5+ tiles and an active speaker is detected
  const useSpotlight = totalTiles >= 5 && activeSpeakerId !== null

  if (useSpotlight) {
    // Find the active speaker peer (or local if it's us)
    const isLocalSpeaker = activeSpeakerId === participantId
    const speakerPeer = !isLocalSpeaker ? peers.find(p => p.id === activeSpeakerId) : null
    const filmstripPeers = peers.filter(p => p.id !== activeSpeakerId)

    return (
      <div data-testid="video-grid" className="flex flex-col h-full gap-2">
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
              videoEnabled={!speakerPeer.videoMuted}
              e2eeActive={e2eeEnabled && e2eePeerStates.get(speakerPeer.id)?.ready}
              isActiveSpeaker
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
                  videoEnabled={!peer.videoMuted}
                  e2eeActive={e2eeEnabled && e2eePeerStates.get(peer.id)?.ready}
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

  // Default grid layout
  const gridCols = (() => {
    if (totalTiles === 1) return 'grid-cols-1'
    if (totalTiles <= 2) return 'grid-cols-2'
    if (totalTiles <= 4) return 'grid-cols-2'
    return 'grid-cols-[repeat(auto-fit,minmax(300px,1fr))]'
  })()

  return (
    <div data-testid="video-grid" className={`grid ${gridCols} gap-2 h-full auto-rows-fr`}>
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
            videoEnabled={!peer.videoMuted}
            e2eeActive={e2eeEnabled && e2eePeerStates.get(peer.id)?.ready}
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
