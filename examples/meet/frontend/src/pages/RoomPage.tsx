import { useMemo } from 'react'
import { useParams, Navigate } from 'react-router-dom'
import { CallProvider } from '@/context/CallContext'
import { VideoGrid } from '@/components/VideoGrid'
import { ControlBar } from '@/components/ControlBar'

export default function RoomPage() {
  const { code } = useParams<{ code: string }>()
  const displayName = sessionStorage.getItem('seameet-display-name')

  const participantId = useMemo(() => crypto.randomUUID(), [])

  if (!displayName || !code) {
    return <Navigate to="/" replace />
  }

  return (
    <CallProvider participantId={participantId} displayName={displayName} roomId={code}>
      <div className="h-dvh flex flex-col">
        {/* Header */}
        <div className="flex items-center px-4 py-2">
          <span className="text-sm text-muted-foreground font-mono">{code}</span>
        </div>

        {/* Video grid */}
        <div className="flex-1 min-h-0 p-2">
          <VideoGrid />
        </div>

        {/* Controls */}
        <ControlBar />
      </div>
    </CallProvider>
  )
}
