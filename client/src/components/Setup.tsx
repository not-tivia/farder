interface SetupProps { onComplete: () => void; }
function Setup({ onComplete }: SetupProps) {
  return (
    <div className="setup">
      <h1>Welcome to Farder</h1>
      <p>Generate your cryptographic identity to get started.</p>
      <button onClick={onComplete}>Generate Identity</button>
    </div>
  );
}
export default Setup;
