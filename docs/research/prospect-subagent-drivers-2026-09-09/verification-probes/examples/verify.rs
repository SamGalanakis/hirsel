use hirsel_drivers::*;
use futures_util::StreamExt;
use tokio::time::{timeout,Duration};
async fn progress(events: &mut EventStream, needle: &str) {
 timeout(Duration::from_secs(2),async { while let Some(e)=events.next().await { if let SubagentEvent::Progress{summary}=e {if summary==needle {return;}} } panic!("closed before barrier"); }).await.expect("barrier deadline");
}
#[tokio::main]
async fn main() {
 let mode=std::env::var("VERIFY_MODE").unwrap();
 let agent=if mode=="exit0" {AgentKind::Claude} else {AgentKind::Codex};
 let driver:Box<dyn SubagentDriver>=if agent==AgentKind::Claude {Box::new(ClaudeCodeDriver::default())} else {Box::new(CodexDriver::default())};
 let spec=SpawnSpec{agent,model:None,variant:None,prompt:"initial".into(),cwd:std::env::current_dir().unwrap(),fake_fixture:None};
 if mode=="startup" {
  assert!(driver.spawn(spec).await.is_err());
  let pid:i32=std::fs::read_to_string(std::env::var("VERIFY_PID").unwrap()).unwrap().parse().unwrap();
  let alive=unsafe {libc::kill(pid,0)==0};
  unsafe {libc::kill(-pid,libc::SIGKILL);}
  assert!(alive,"expected startup ownership gap");
  println!("CONFIRMED startup returned error while fixture process remained alive; fixture group killed"); return;
 }
 let h=driver.spawn(spec).await.unwrap();
 let mut events=driver.events(&h).unwrap();
 if mode=="child" {
  let result=timeout(Duration::from_secs(2),async {while let Some(e)=events.next().await {if let SubagentEvent::Terminal{outcome}=e {return outcome;}} panic!("closed");}).await.unwrap();
  assert!(matches!(result,TerminalOutcome::Done{..}));
  println!("CONFIRMED captured native CHILD turn/completed produces root driver Done before root completion");
 } else if mode=="exit0" {
  assert!(timeout(Duration::from_secs(2),async {while let Some(e)=events.next().await {if matches!(e,SubagentEvent::Terminal{..}) {return;}}}).await.is_err());
  let pid:i32=std::fs::read_to_string(std::env::var("VERIFY_PID").unwrap()).unwrap().parse().unwrap();
  assert_eq!(unsafe{libc::kill(pid,0)},-1,"fixture must actually have exited");
  println!("CONFIRMED clean-zero child exited; subscribed driver stream neither closed nor emitted terminal within 2s");
 } else {
  progress(&mut events,"barrier-A").await;
  driver.prompt(&h,"followup".into()).await.expect("prompt reports success");
  progress(&mut events,if mode=="reject" {"barrier-rejected"} else {"barrier-B-queued"}).await;
  driver.interrupt(&h).await.expect("interrupt reports success");
  progress(&mut events,if mode=="reject" {"interrupt-target-A"} else {"interrupt-target-B"}).await;
  println!("CONFIRMED {mode}: prompt/interrupt report success; {}",if mode=="reject" {"rejected turn/start error is ignored"}else{"interrupt targets queued B while A is active"});
 }
 driver.retire(&h).await.unwrap();
}
