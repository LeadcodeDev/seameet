import { BrowserRouter, Routes, Route } from 'react-router-dom'
import { TooltipProvider } from '@/components/ui/tooltip'
import LobbyPage from '@/pages/LobbyPage'
import RoomPage from '@/pages/RoomPage'

export default function App() {
  return (
    <BrowserRouter>
      <TooltipProvider>
        <Routes>
          <Route path="/" element={<LobbyPage />} />
          <Route path="/room/:code" element={<RoomPage />} />
        </Routes>
      </TooltipProvider>
    </BrowserRouter>
  )
}
