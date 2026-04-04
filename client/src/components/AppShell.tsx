import TitleBar from "./TitleBar";
import ChannelSidebar from "./ChannelSidebar";
import ChatPanel from "./ChatPanel";
import MemberSidebar from "./MemberSidebar";

export default function AppShell() {
  return (
    <>
      <TitleBar />
      <div className="main-layout">
        <ChannelSidebar />
        <ChatPanel />
        <MemberSidebar />
      </div>
    </>
  );
}
