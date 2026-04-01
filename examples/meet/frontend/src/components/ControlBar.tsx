import { Button } from "@/components/ui/button";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import { VideoSettingsPopover } from "@/components/VideoSettingsPopover";
import { useCall } from "@/context/CallContext";
import {
  Check,
  Copy,
  Mic,
  MicOff,
  Monitor,
  Phone,
  ShieldAlert,
  ShieldCheck,
  Video,
  VideoOff,
} from "lucide-react";
import { useCallback, useState } from "react";

export function ControlBar() {
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
    e2eePeerStates,
    remotePeers,
  } = useCall();

  const allPeersE2EE = e2eeEnabled && remotePeers.size > 0 &&
    Array.from(remotePeers.keys()).every(id => e2eePeerStates.get(id)?.ready);
  const someNotReady = e2eeEnabled && remotePeers.size > 0 && !allPeersE2EE;

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

      {/* E2EE indicator */}
      {e2eeEnabled && (
        <Tooltip>
          <TooltipTrigger asChild>
            <div
              data-testid="e2ee-indicator"
              className={`flex items-center justify-center h-12 w-12 rounded-full ${
                someNotReady
                  ? "bg-orange-500/20 text-orange-400"
                  : "bg-green-500/20 text-green-400"
              }`}
            >
              {someNotReady ? (
                <ShieldAlert className="h-5 w-5" />
              ) : (
                <ShieldCheck className="h-5 w-5" />
              )}
            </div>
          </TooltipTrigger>
          <TooltipContent>
            {someNotReady
              ? "E2EE active — some participants not yet secured"
              : "End-to-end encrypted"}
          </TooltipContent>
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
