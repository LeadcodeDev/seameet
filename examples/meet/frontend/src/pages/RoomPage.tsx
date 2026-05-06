import { useState, useCallback } from 'react'
import { useParams, useLocation, Navigate, useNavigate } from 'react-router-dom'
import { CallProvider, useCall } from '@/context/CallContext'
import { ErrorBoundary } from '@/components/ErrorBoundary'
import { VideoGrid } from '@/components/VideoGrid'
import { ControlBar } from '@/components/ControlBar'
import { ChatPanel } from '@/components/ChatPanel'
import { useRoomSession } from '@/hooks/useRoomSession'

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

function SessionGate({
  status,
  error,
  onRetry,
  onLeave,
}: {
  status: 'idle' | 'creating' | 'error'
  error: string | null
  onRetry: () => void
  onLeave: () => void
}) {
  return (
    <div className="h-dvh flex items-center justify-center bg-background">
      <div className="max-w-md w-full p-6 rounded-lg border bg-card text-card-foreground space-y-4 text-center">
        {status === 'creating' && (
          <>
            <div className="text-lg font-medium">Préparation de la room…</div>
            <div className="text-sm text-muted-foreground">
              Création de votre identité côté serveur.
            </div>
          </>
        )}
        {status === 'error' && (
          <>
            <div className="text-lg font-medium text-destructive">
              Impossible de rejoindre la room
            </div>
            <div className="text-sm text-muted-foreground">{error ?? 'Erreur inconnue'}</div>
            <div className="flex gap-2 justify-center">
              <button
                className="px-4 py-2 rounded-md bg-primary text-primary-foreground hover:bg-primary/90"
                onClick={onRetry}
              >
                Réessayer
              </button>
              <button
                className="px-4 py-2 rounded-md border hover:bg-accent"
                onClick={onLeave}
              >
                Retour
              </button>
            </div>
          </>
        )}
      </div>
    </div>
  )
}

export default function RoomPage() {
  const { code } = useParams<{ code: string }>()
  const location = useLocation()
  const navigate = useNavigate()
  const displayName = sessionStorage.getItem('seameet-display-name')

  const lobbyState = location.state as
    | { cameraOn?: boolean; micOn?: boolean; e2eeOn?: boolean }
    | null

  const { session, status, error, retry } = useRoomSession(code, displayName ?? undefined)

  if (!displayName || !code) {
    return <Navigate to="/" replace />
  }

  if (status !== 'ready' || !session) {
    return (
      <SessionGate
        status={status === 'ready' ? 'creating' : status}
        error={error}
        onRetry={retry}
        onLeave={() => navigate('/')}
      />
    )
  }

  return (
    <ErrorBoundary>
      <CallProvider
        participantId={session.participantId}
        sessionToken={session.sessionToken}
        displayName={displayName}
        roomId={code}
        initialAudioEnabled={lobbyState?.micOn}
        initialVideoEnabled={lobbyState?.cameraOn}
        initialE2EEEnabled={true}
      >
        <RoomContent />
      </CallProvider>
    </ErrorBoundary>
  )
}
