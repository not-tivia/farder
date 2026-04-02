import { useState } from "react";
import Setup from "./components/Setup";
import Chat from "./components/Chat";

function App() {
  const [hasIdentity, setHasIdentity] = useState(false);
  return (
    <div className="app">
      {hasIdentity ? <Chat /> : <Setup onComplete={() => setHasIdentity(true)} />}
    </div>
  );
}
export default App;
