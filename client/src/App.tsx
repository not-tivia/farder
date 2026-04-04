import { ServerProvider, useServer } from "./context/ServerContext";
import { useServerEvents } from "./hooks/useServerEvents";
import ConnectDialog from "./components/ConnectDialog";
import AppShell from "./components/AppShell";

function AppInner() {
  const { state } = useServer();
  useServerEvents();
  return state.connected ? <AppShell /> : <ConnectDialog />;
}

export default function App() {
  return (
    <ServerProvider>
      <AppInner />
    </ServerProvider>
  );
}
