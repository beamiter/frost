#!/usr/bin/env python3
"""Run current task subscription admission against explicit runtime fixtures.

Extracts the production Tasks pump branch; GUI subscriptions and worker state
are test doubles. --check-regression also requires former idle polling to fail.
"""
from pathlib import Path
import argparse
import os
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]


def run():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check-regression", action="store_true")
    args = parser.parse_args()
    source = (ROOT / "src/main.rs").read_text()
    # Session-library discovery and durable organization have their own pump.
    # Extract only the Tasks branch represented by the runtime fixtures below.
    branch = source.split("        let tasks_open =", 1)[1].split("        if self.agent_sessions.open", 1)[0]
    branch = "        let tasks_open =" + branch
    fixture = r'''
#![allow(dead_code)]
use std::time::Duration;
mod iced {pub mod time {pub fn every(d:std::time::Duration)->super::Subscription{super::Subscription(d)}}pub struct Subscription(pub std::time::Duration);impl Subscription{pub fn map<F:Fn(())->crate::Message>(self,_:F)->std::time::Duration{self.0}}}
enum Message{TaskTick}
#[derive(Clone,Copy,PartialEq)]enum SidebarPanel{Tasks,Files}
struct Config{experimental_task_sidebar:bool}
struct Runtime{activity:bool,fast:bool}impl Runtime{fn has_any_activity(&self)->bool{self.activity}fn needs_fast_poll(&self)->bool{self.fast}}
struct Diff{is_open:bool,loading:bool}impl Diff{fn state(&self)->&Self{self}}
struct Panel{pending_creation:Option<()>,diff:Diff,prompt_poll:bool}impl Panel{fn prompt_poll_active(&self)->bool{self.prompt_poll}}
struct App{focused:bool,config:Config,sidebar_open:bool,sidebar_panel:SidebarPanel,agent_runtime:Runtime,task_panel:Panel}
impl App{fn dock_open(&self)->bool{self.sidebar_open}fn interval(&self)->Option<Duration>{let mut subs=Vec::new(); BODY assert!(subs.len()<=1);subs.first().copied()}}
fn app()->App{App{focused:true,config:Config{experimental_task_sidebar:true},sidebar_open:true,sidebar_panel:SidebarPanel::Tasks,agent_runtime:Runtime{activity:false,fast:false},task_panel:Panel{pending_creation:None,diff:Diff{is_open:true,loading:false},prompt_poll:false}}}
#[test]fn completed_diff_does_not_poll_hidden_panel(){for visible in [false,true]{let mut a=app();a.sidebar_open=visible;if visible{a.sidebar_panel=SidebarPanel::Files;}assert_eq!(a.interval(),None);}}
#[test]fn parked_panel_stops_on_blur_and_resumes_on_focus(){let mut a=app();assert_eq!(a.interval(),Some(Duration::from_millis(500)));a.focused=false;assert_eq!(a.interval(),None);a.focused=true;assert_eq!(a.interval(),Some(Duration::from_millis(500)));}
#[test]fn disabled_feature_has_no_display_poll(){let mut a=app();a.config.experimental_task_sidebar=false;assert_eq!(a.interval(),None);}
#[test]fn background_work_keeps_original_poll_rates(){for source in 0..4{let mut a=app();a.focused=false;a.sidebar_open=false;a.config.experimental_task_sidebar=false;match source{0=>a.agent_runtime.activity=true,1=>{a.agent_runtime.activity=true;a.agent_runtime.fast=true;},2=>a.task_panel.pending_creation=Some(()),_=>a.task_panel.diff.loading=true,};assert_eq!(a.interval(),Some(Duration::from_millis(if source==0{500}else{50})));}}
#[test]fn hidden_worker_completion_retires_tick_without_reopening_ui(){for source in 0..3{let mut a=app();a.sidebar_open=false;match source{0=>a.agent_runtime.activity=true,1=>a.task_panel.pending_creation=Some(()),_=>a.task_panel.diff.loading=true};assert!(a.interval().is_some());a.agent_runtime.activity=false;a.task_panel.pending_creation=None;a.task_panel.diff.loading=false;assert_eq!(a.interval(),None);assert!(!a.sidebar_open);}}
#[test]fn user_action_burst_is_fast_only_while_work_is_admitted(){let mut a=app();a.focused=false;a.sidebar_open=false;a.task_panel.prompt_poll=true;assert_eq!(a.interval(),None);a.agent_runtime.activity=true;assert_eq!(a.interval(),Some(Duration::from_millis(50)));a.task_panel.prompt_poll=false;assert_eq!(a.interval(),Some(Duration::from_millis(500)));}
#[test]fn visibility_matrix_preserves_business_polling(){for bits in 0..512{let mut a=app();a.focused=bits&1!=0;a.sidebar_open=bits&2!=0;a.config.experimental_task_sidebar=bits&4!=0;a.sidebar_panel=if bits&8!=0{SidebarPanel::Tasks}else{SidebarPanel::Files};a.agent_runtime.activity=bits&16!=0;a.agent_runtime.fast=a.agent_runtime.activity && bits&32!=0;a.task_panel.pending_creation=(bits&64!=0).then_some(());a.task_panel.diff.loading=bits&128!=0;a.task_panel.prompt_poll=bits&256!=0;let business=a.agent_runtime.activity||a.task_panel.pending_creation.is_some()||a.task_panel.diff.loading;let visible=a.focused&&a.sidebar_open&&a.config.experimental_task_sidebar&&a.sidebar_panel==SidebarPanel::Tasks;let expected=(business||visible).then_some(Duration::from_millis(if a.agent_runtime.fast||a.task_panel.prompt_poll||a.task_panel.pending_creation.is_some()||a.task_panel.diff.loading{50}else{500}));assert_eq!(a.interval(),expected,"flags={bits}");}}
'''
    variants = [("current", branch)]
    if args.check_regression:
        old = branch.replace("self.focused\n            && self.config.experimental_task_sidebar\n            && self.dock_open()", "self.sidebar_open")
        old = old.replace("|| self.task_panel.diff.state().loading", "|| self.task_panel.diff.is_open\n            || self.task_panel.diff.state().loading", 1)
        assert old != branch
        variants.append(("regression", old))
    with tempfile.TemporaryDirectory(prefix="frost-task-polling-") as temp:
        temp = Path(temp)
        for name, body in variants:
            path = temp / (name + ".rs")
            path.write_text(fixture.replace("BODY", body))
            binary = temp / name
            subprocess.run([os.environ.get("RUSTC", "rustc"), "--edition=2021", "--test", str(path), "-o", str(binary)], check=True)
            result = subprocess.run([str(binary)], capture_output=True, text=True)
            print(name + ":\n" + result.stdout)
            if name == "current":
                assert result.returncode == 0, result.stderr
            else:
                assert result.returncode != 0
                assert "completed_diff_does_not_poll_hidden_panel ... FAILED" in result.stdout
                assert "background_work_keeps_original_poll_rates ... ok" in result.stdout
    print("Task polling admission regression checks passed.")


if __name__ == "__main__":
    run()
