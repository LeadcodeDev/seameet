import { useState, useEffect, useRef } from 'react'
import { useNavigate, useParams } from 'react-router-dom'
import { Card, CardHeader, CardTitle, CardDescription, CardContent, CardFooter } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Button } from '@/components/ui/button'
import { Video, VideoOff, Mic, MicOff } from 'lucide-react'

const ADJECTIVES = [
  'blue', 'red', 'green', 'happy', 'calm', 'bold', 'warm', 'cool',
  'fast', 'soft', 'bright', 'dark', 'fresh', 'wild', 'kind', 'free',
]

const NOUNS = [
  'cat', 'dog', 'fox', 'owl', 'bear', 'wolf', 'hawk', 'deer',
  'lion', 'fish', 'duck', 'frog', 'swan', 'crow', 'moth', 'seal',
]

function generateRoomCode(): string {
  const pick = (arr: string[]) => arr[Math.floor(Math.random() * arr.length)]
  return `${pick(ADJECTIVES)}-${pick(ADJECTIVES)}-${pick(NOUNS)}`
}

export default function LobbyPage() {
  const { code } = useParams<{ code?: string }>()
  const navigate = useNavigate()

  const [displayName, setDisplayName] = useState(() => sessionStorage.getItem('seameet-display-name') ?? '')
  const [roomCode, setRoomCode] = useState(code ?? '')
  const [cameraOn, setCameraOn] = useState(false)
  const [micOn, setMicOn] = useState(false)
  const videoRef = useRef<HTMLVideoElement>(null)
  const streamRef = useRef<MediaStream | null>(null)

  // Camera preview — start/stop based on cameraOn toggle
  useEffect(() => {
    if (!cameraOn) {
      // Stop existing preview
      if (streamRef.current) {
        streamRef.current.getTracks().forEach((t) => t.stop())
        streamRef.current = null
      }
      if (videoRef.current) {
        videoRef.current.srcObject = null
      }
      return
    }

    let stream: MediaStream | null = null
    let cancelled = false

    async function startPreview() {
      try {
        stream = await navigator.mediaDevices.getUserMedia({ video: true, audio: false })
        if (cancelled) {
          stream.getTracks().forEach((t) => t.stop())
          return
        }
        streamRef.current = stream
        if (videoRef.current) {
          videoRef.current.srcObject = stream
        }
      } catch {
        // Camera not available, preview will be blank
      }
    }

    startPreview()

    return () => {
      cancelled = true
      if (stream) {
        stream.getTracks().forEach((t) => t.stop())
      }
    }
  }, [cameraOn])

  function handleJoin(e: React.FormEvent) {
    e.preventDefault()
    if (!displayName.trim()) return

    const finalCode = roomCode.trim() || generateRoomCode()
    sessionStorage.setItem('seameet-display-name', displayName.trim())
    // Stop lobby preview stream before navigating — useMediaDevices will acquire its own
    if (streamRef.current) {
      streamRef.current.getTracks().forEach((t) => t.stop())
      streamRef.current = null
    }
    navigate(`/room/${finalCode}`, { state: { cameraOn, micOn } })
  }

  return (
    <div className="min-h-dvh flex items-center justify-center p-4">
      <Card className="w-full max-w-md">
        <CardHeader className="text-center">
          <CardTitle className="text-2xl font-bold tracking-tight">
            seameet
          </CardTitle>
          <CardDescription>
            {code ? `Join room: ${code}` : 'Video calls for everyone'}
          </CardDescription>
        </CardHeader>

        <CardContent>
          <form onSubmit={handleJoin} className="space-y-4">
            {/* Camera preview */}
            <div className="relative mx-auto w-48 h-36 rounded-lg overflow-hidden bg-secondary">
              {cameraOn ? (
                <video
                  ref={videoRef}
                  autoPlay
                  playsInline
                  muted
                  className="w-full h-full object-cover"
                />
              ) : (
                <div className="w-full h-full flex items-center justify-center">
                  <VideoOff className="w-8 h-8 text-muted-foreground" />
                </div>
              )}
            </div>

            {/* Media toggles */}
            <div className="flex justify-center gap-2">
              <Button
                type="button"
                variant={micOn ? 'default' : 'secondary'}
                size="icon"
                onClick={() => setMicOn((v) => !v)}
              >
                {micOn ? <Mic className="w-4 h-4" /> : <MicOff className="w-4 h-4" />}
              </Button>
              <Button
                type="button"
                variant={cameraOn ? 'default' : 'secondary'}
                size="icon"
                onClick={() => setCameraOn((v) => !v)}
              >
                {cameraOn ? <Video className="w-4 h-4" /> : <VideoOff className="w-4 h-4" />}
              </Button>
            </div>

            <div className="space-y-2">
              <Input
                placeholder="Your name"
                value={displayName}
                onChange={(e) => setDisplayName(e.target.value)}
                required
                autoFocus
              />
            </div>

            <div className="space-y-2">
              <Input
                placeholder="Room code (leave empty to auto-generate)"
                value={roomCode}
                onChange={(e) => setRoomCode(e.target.value)}
              />
            </div>

            <Button type="submit" className="w-full" disabled={!displayName.trim()}>
              Join
            </Button>
          </form>
        </CardContent>

        <CardFooter className="justify-center">
          <p className="text-xs text-muted-foreground">
            Powered by seameet SFU
          </p>
        </CardFooter>
      </Card>
    </div>
  )
}
