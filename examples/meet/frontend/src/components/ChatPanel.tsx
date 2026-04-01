import { useRef, useEffect, useState, useCallback, type KeyboardEvent } from 'react'
import { Lock, Send, X } from 'lucide-react'
import { Button } from '@/components/ui/button'

export interface ChatMessage {
  id: string
  from: string
  displayName: string
  content: string
  timestamp: number
  encrypted?: boolean
}

interface ChatPanelProps {
  messages: ChatMessage[]
  onSend: (content: string) => void
  onClose: () => void
  participantId: string
}

export function ChatPanel({ messages, onSend, onClose, participantId }: ChatPanelProps) {
  const [input, setInput] = useState('')
  const scrollRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    scrollRef.current?.scrollTo({ top: scrollRef.current.scrollHeight, behavior: 'smooth' })
  }, [messages.length])

  const handleSend = useCallback(() => {
    const trimmed = input.trim()
    if (!trimmed) return
    onSend(trimmed)
    setInput('')
  }, [input, onSend])

  const handleKeyDown = useCallback((e: KeyboardEvent) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault()
      handleSend()
    }
  }, [handleSend])

  return (
    <div className="flex flex-col h-full w-80 bg-[#202124] border-l border-[#3c4043]">
      {/* Header */}
      <div className="flex items-center justify-between px-4 py-3 border-b border-[#3c4043]">
        <span className="text-sm font-medium text-white">Chat</span>
        <Button variant="ghost" size="icon" className="h-8 w-8 text-gray-400 hover:text-white" onClick={onClose}>
          <X className="h-4 w-4" />
        </Button>
      </div>

      {/* Messages */}
      <div ref={scrollRef} className="flex-1 overflow-y-auto px-4 py-2 space-y-3">
        {messages.length === 0 && (
          <p className="text-sm text-gray-500 text-center mt-8">No messages yet</p>
        )}
        {messages.map((msg) => {
          const isOwn = msg.from === participantId
          return (
            <div key={msg.id} className={`flex flex-col ${isOwn ? 'items-end' : 'items-start'}`}>
              {!isOwn && (
                <span className="text-xs text-gray-400 mb-0.5">{msg.displayName}</span>
              )}
              <div className={`max-w-[85%] rounded-lg px-3 py-1.5 text-sm break-words ${
                isOwn ? 'bg-blue-600 text-white' : 'bg-[#3c4043] text-white'
              } ${msg.encrypted ? 'italic text-gray-400' : ''}`}>
                {msg.encrypted ? (
                  <span className="inline-flex items-center gap-1">
                    <Lock className="h-3 w-3" />
                    {msg.content}
                  </span>
                ) : msg.content}
              </div>
              <span className="text-[10px] text-gray-500 mt-0.5">
                {new Date(msg.timestamp).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}
              </span>
            </div>
          )
        })}
      </div>

      {/* Input */}
      <div className="px-3 py-3 border-t border-[#3c4043]">
        <div className="flex items-center gap-2">
          <input
            type="text"
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={handleKeyDown}
            placeholder="Send a message..."
            className="flex-1 bg-[#3c4043] text-white text-sm rounded-lg px-3 py-2 outline-none placeholder-gray-500 focus:ring-1 focus:ring-blue-500"
          />
          <Button
            variant="ghost"
            size="icon"
            className="h-8 w-8 text-blue-400 hover:text-blue-300"
            onClick={handleSend}
            disabled={!input.trim()}
          >
            <Send className="h-4 w-4" />
          </Button>
        </div>
      </div>
    </div>
  )
}
