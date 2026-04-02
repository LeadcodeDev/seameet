import { useMemo, useState, useCallback } from 'react'
import { useParams, useLocation, Navigate } from 'react-router-dom'
import { CallProvider, useCall } from '@/context/CallContext'
import { ErrorBoundary } from '@/components/ErrorBoundary'
import { VideoGrid } from '@/components/VideoGrid'
import { ControlBar } from '@/components/ControlBar'
import { ChatPanel } from '@/components/ChatPanel'

function RoomContent() {
  const { chatMessages, sendChatMessage, participantId, mediaError, roomId } = useCall()
  const [chatOpen, setChatOpen] = useState(false)

  const toggleChat = useCallback(() => setChatOpen(prev => !prev), [])

  return (
    <div className="h-dvh flex flex-col">
      {/* Media error banner */}
      {mediaError && (
        <div className="bg-destructive/10 border-b border-destructive/20 px-4 py-2 text-sm text-destructive">
          Camera/microphone unavailable: {mediaError}
        </div>
      )}

      {/* Header */}
      <div className="flex items-center px-4 py-2">
        <span className="text-sm text-muted-foreground font-mono">{roomId}</span>
      </div>

      {/* Main content area */}
      <div className="flex-1 min-h-0 flex">
        {/* Video grid */}
        <div className="flex-1 min-h-0 p-2">
          <VideoGrid />
        </div>

        {/* Chat sidebar */}
        {chatOpen && (
          <ChatPanel
            messages={chatMessages}
            onSend={sendChatMessage}
            onClose={toggleChat}
            participantId={participantId}
          />
        )}
      </div>

      {/* Controls */}
      <ControlBar onToggleChat={toggleChat} chatOpen={chatOpen} />
    </div>
  )
}

export default function RoomPage() {
  const { code } = useParams<{ code: string }>()
  const location = useLocation()
  const displayName = sessionStorage.getItem('seameet-display-name')

  const participantId = useMemo(() => crypto.randomUUID(), [])

  const lobbyState = location.state as { cameraOn?: boolean; micOn?: boolean; e2eeOn?: boolean } | null

  if (!displayName || !code) {
    return <Navigate to="/" replace />
  }

  return (
    <ErrorBoundary>
      <CallProvider
        participantId={participantId}
        displayName={displayName}
        roomId={code}
        initialAudioEnabled={lobbyState?.micOn}
        initialVideoEnabled={lobbyState?.cameraOn}
        initialE2EEEnabled={lobbyState?.e2eeOn}
      >
        <RoomContent />
      </CallProvider>
    </ErrorBoundary>
  )
}
