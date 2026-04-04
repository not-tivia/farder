const COMMON_EMOJI = [
  "👍", "👎", "❤️", "😂", "😮", "😢", "😡", "🎉",
  "🔥", "✅", "❌", "🙏", "👀", "💯", "🤔", "⭐",
];

interface ReactionPickerProps {
  onSelect: (emoji: string) => void;
}

export default function ReactionPicker({ onSelect }: ReactionPickerProps) {
  return (
    <div className="reaction-picker">
      {COMMON_EMOJI.map((emoji) => (
        <button
          key={emoji}
          className="reaction-picker-btn"
          onClick={() => onSelect(emoji)}
          title={emoji}
        >
          {emoji}
        </button>
      ))}
    </div>
  );
}
