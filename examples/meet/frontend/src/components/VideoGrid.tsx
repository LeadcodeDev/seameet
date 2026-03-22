import { useMemo } from 'react'
import { useCall } from '@/context/CallContext'
import { VideoTile } from '@/components/VideoTile'

export function VideoGrid() {
  const { localStream, remotePeers, displayName, audioEnabled, videoEnabled } = useCall()

  const peers = useMemo(() => Array.from(remotePeers.values()), [remotePeers])
  const totalParticipants = 1 + peers.length

  const gridCols = useMemo(() => {
    if (totalParticipants === 1) return 'grid-cols-1'
    if (totalParticipants <= 2) return 'grid-cols-2'
    if (totalParticipants <= 4) return 'grid-cols-2'
    return 'grid-cols-[repeat(auto-fit,minmax(300px,1fr))]'
  }, [totalParticipants])

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

      {/* Remote peers */}
      {peers.map((peer) => (
        <VideoTile
          key={peer.id}
          stream={peer.stream}
          name={peer.displayName}
          isLocal={false}
          audioEnabled
          videoEnabled
        />
      ))}
    </div>
  )
}
