export default function JoinConfirmModal({
  onConfirm,
  onCancel,
}: {
  onConfirm: () => void;
  onCancel: () => void;
}) {
  return (
    <div className="modal-overlay" onClick={onCancel}>
      <div className="modal-dialog" onClick={(e) => e.stopPropagation()}>
        <div className="modal-titlebar">
          <span>Join server</span>
          <button className="modal-close" onClick={onCancel}>X</button>
        </div>
        <div className="modal-body">
          <p>You've been invited to a Farder server. Join it?</p>
          <div className="connect-actions">
            <button className="xp-button" onClick={onConfirm}>Join</button>
            <button className="xp-button" onClick={onCancel}>Cancel</button>
          </div>
        </div>
      </div>
    </div>
  );
}
