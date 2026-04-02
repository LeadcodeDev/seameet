import { Button } from "@/components/ui/button";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import { VideoSettingsPopover } from "@/components/VideoSettingsPopover";
import { SafetyNumberPanel } from "@/components/SafetyNumberPanel";
import { useCall } from "@/context/CallContext";
import {
  Check,
  Copy,
  MessageSquare,
  Mic,
  MicOff,
  Monitor,
  Phone,
  Video,
  VideoOff,
} from "lucide-react";
import { useCallback, useState } from "react";

interface ControlBarProps {
  onToggleChat?: () => void
  chatOpen?: boolean
}

export function ControlBar({ onToggleChat, chatOpen }: ControlBarProps) {
  const {
    audioEnabled,
    videoEnabled,
    toggleAudio,
    toggleVideo,
    startScreenShare,
    stopScreenShare,
    localScreenStream,
    leave,
    roomId,
    e2eeEnabled,
  } = useCall();

  const [copied, setCopied] = useState(false);

  const handleCopyLink = useCallback(async () => {
    const url = `${window.location.origin}/room/${roomId}`;
    await navigator.clipboard.writeText(url);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  }, [roomId]);

  const handleScreenShare = useCallback(async () => {
    if (localScreenStream) {
      await stopScreenShare();
    } else {
      try {
        await startScreenShare();
      } catch {
        // User cancelled or error
      }
    }
  }, [localScreenStream, startScreenShare, stopScreenShare]);

  return (
    <div className="flex items-center justify-center gap-3 p-4">
      {/* Mic toggle */}
      <Tooltip>
        <TooltipTrigger asChild>
          <Button
            data-testid="btn-toggle-mic"
            variant={audioEnabled ? "secondary" : "destructive"}
            size="icon"
            className="h-12 w-12 rounded-full"
            onClick={toggleAudio}
          >
            {audioEnabled ? (
              <Mic className="h-5 w-5" />
            ) : (
              <MicOff className="h-5 w-5" />
            )}
          </Button>
        </TooltipTrigger>
        <TooltipContent>{audioEnabled ? "Mute" : "Unmute"}</TooltipContent>
      </Tooltip>

      {/* Camera toggle */}
      <Tooltip>
        <TooltipTrigger asChild>
          <Button
            data-testid="btn-toggle-camera"
            variant={videoEnabled ? "secondary" : "destructive"}
            size="icon"
            className="h-12 w-12 rounded-full"
            onClick={toggleVideo}
          >
            {videoEnabled ? (
              <Video className="h-5 w-5" />
            ) : (
              <VideoOff className="h-5 w-5" />
            )}
          </Button>
        </TooltipTrigger>
        <TooltipContent>
          {videoEnabled ? "Turn off camera" : "Turn on camera"}
        </TooltipContent>
      </Tooltip>

      {/* Screen share */}
      <Tooltip>
        <TooltipTrigger asChild>
          <Button
            data-testid="btn-screen-share"
            variant={localScreenStream ? "default" : "secondary"}
            size="icon"
            className="h-12 w-12 rounded-full"
            onClick={handleScreenShare}
          >
            <Monitor className="h-5 w-5" />
          </Button>
        </TooltipTrigger>
        <TooltipContent>
          {localScreenStream ? "Stop sharing" : "Share screen"}
        </TooltipContent>
      </Tooltip>

      {/* E2EE indicator with safety numbers */}
      {e2eeEnabled && <SafetyNumberPanel />}

      {/* Chat */}
      {onToggleChat && (
        <Tooltip>
          <TooltipTrigger asChild>
            <Button
              data-testid="btn-toggle-chat"
              variant={chatOpen ? "default" : "secondary"}
              size="icon"
              className="h-12 w-12 rounded-full"
              onClick={onToggleChat}
            >
              <MessageSquare className="h-5 w-5" />
            </Button>
          </TooltipTrigger>
          <TooltipContent>{chatOpen ? "Close chat" : "Open chat"}</TooltipContent>
        </Tooltip>
      )}

      {/* Video settings */}
      <VideoSettingsPopover />

      {/* Copy room link */}
      <Tooltip>
        <TooltipTrigger asChild>
          <Button
            variant="secondary"
            size="icon"
            className="h-12 w-12 rounded-full"
            onClick={handleCopyLink}
          >
            {copied ? (
              <Check className="h-5 w-5" />
            ) : (
              <Copy className="h-5 w-5" />
            )}
          </Button>
        </TooltipTrigger>
        <TooltipContent>{copied ? "Copied!" : "Copy room link"}</TooltipContent>
      </Tooltip>

      {/* Leave */}
      <Tooltip>
        <TooltipTrigger asChild>
          <Button
            data-testid="btn-leave"
            variant="destructive"
            className="h-12 rounded-full px-6"
            onClick={leave}
          >
            <Phone className="h-5 w-5 rotate-135" />
          </Button>
        </TooltipTrigger>
        <TooltipContent>Leave call</TooltipContent>
      </Tooltip>
    </div>
  );
}
