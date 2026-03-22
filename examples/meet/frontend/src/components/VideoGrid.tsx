import React, { useMemo } from 'react'
import { useCall } from '@/context/CallContext'
import { VideoTile } from '@/components/VideoTile'

export function VideoGrid() {
  const { localStream, remotePeers, displayName, audioEnabled, videoEnabled, localScreenStream } = useCall()

  const peers = useMemo(() => Array.from(remotePeers.values()), [remotePeers])
  const screenShareCount = (localScreenStream ? 1 : 0) + peers.filter(p => p.screenStream).length
  const totalTiles = 1 + peers.length + screenShareCount

  const gridCols = useMemo(() => {
    if (totalTiles === 1) return 'grid-cols-1'
    if (totalTiles <= 2) return 'grid-cols-2'
    if (totalTiles <= 4) return 'grid-cols-2'
    return 'grid-cols-[repeat(auto-fit,minmax(300px,1fr))]'
  }, [totalTiles])

  return (
    <div className={`grid ${gridCols} gap-2 h-full auto-rows-fr`}>
      {/* Local video */}
      <VideoTile
        stream={localStream}
        name={displayName}
        isLocal
        audioEnabled={audioEnabled}
        videoEnabled={videoEnabled}
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
            audioEnabled
            videoEnabled
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
