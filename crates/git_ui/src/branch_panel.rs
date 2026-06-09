use git::repository::{Branch, BranchesScanResult, UpstreamTracking};
use gpui::{
    Action, App, AsyncWindowContext, Context, DismissEvent, Entity, EventEmitter, FocusHandle,
    Focusable, IntoElement, ParentElement, Render, SharedString, Styled, Subscription,
    WeakEntity, Window, actions, prelude::*,
};
use project::git_store::{GitStore, GitStoreEvent, Repository};
use time::{Duration, OffsetDateTime};
use ui::{Color, ContextMenu, Divider, Icon, IconName, Label, Tooltip, prelude::*};
use workspace::{Workspace, dock::{DockPosition, Panel, PanelEvent}};

const BRANCH_PANEL_KEY: &str = "BranchPanel";

actions!(branch_panel, [ToggleFocus]);

struct BranchContextMenu {
    menu: Entity<ContextMenu>,
    _branch_name: SharedString,
}

pub struct BranchPanel {
    focus_handle: FocusHandle,
    git_store: Entity<GitStore>,
    workspace: WeakEntity<Workspace>,
    active_repo: Option<Entity<Repository>>,
    branches: Vec<Branch>,
    error: Option<SharedString>,
    is_loading: bool,
    local_expanded: bool,
    remote_expanded: bool,
    context_menu: Option<BranchContextMenu>,
    _subscriptions: Vec<Subscription>,
}

impl BranchPanel {
    pub async fn load(
        workspace: WeakEntity<Workspace>,
        mut cx: AsyncWindowContext,
    ) -> anyhow::Result<Entity<Self>> {
        workspace.update_in(&mut cx, |workspace, window, cx| {
            let git_store = workspace
                .project()
                .read(cx)
                .git_store()
                .clone();
            cx.new(|cx| Self::new(workspace, git_store, window, cx))
        })
    }

    pub fn new(
        workspace: &Workspace,
        git_store: Entity<GitStore>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let _ = window;

        let active_repo = git_store.read(cx).active_repository().clone();

        let mut subscriptions = Vec::new();
        subscriptions.push(cx.subscribe(&git_store, |this, _, event, cx| {
            if let GitStoreEvent::ActiveRepositoryChanged(_) = event {
                this.active_repo = this.git_store.read(cx).active_repository().clone();
                this.refresh_branches(cx);
            }
        }));

        let mut this = Self {
            focus_handle,
            git_store,
            workspace: workspace.weak_handle(),
            active_repo,
            branches: Vec::new(),
            error: None,
            is_loading: false,
            local_expanded: true,
            remote_expanded: true,
            context_menu: None,
            _subscriptions: subscriptions,
        };
        this.refresh_branches(cx);
        this
    }

    fn refresh_branches(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.active_repo.clone() else {
            self.branches.clear();
            self.error = None;
            cx.notify();
            return;
        };

        self.is_loading = true;
        cx.notify();

        let receiver = repo.update(cx, |repo, _| repo.branches());
        cx.spawn(async move |this, cx| {
            let result = receiver.await;
            this.update(cx, |this, cx| {
                this.is_loading = false;
                match result {
                    Ok(Ok(BranchesScanResult { branches, error })) => {
                        this.branches = branches;
                        this.error = error;
                    }
                    Ok(Err(error)) => {
                        this.error = Some(error.to_string().into());
                    }
                    Err(_) => {
                        this.error = Some("Failed to load branches".into());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn deploy_branch_context_menu(
        &mut self,
        branch: &Branch,
        _position: gpui::Point<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let branch_name = SharedString::from(branch.name().to_string());
        let is_remote = branch.is_remote();
        let is_head = branch.is_head;
        let has_upstream = branch.upstream.is_some();

        let repo = self.active_repo.clone();
        let this = cx.entity();

        let context_menu = ContextMenu::build(window, cx, {
            let repo = repo.clone();
            let branch_name_for_menu = branch_name.clone();
            move |menu, window, _cx| {
                let menu = menu.header(branch_name_for_menu.clone());

                let menu = if !is_remote && !is_head {
                    let name = branch_name_for_menu.clone();
                    let _repo = repo.clone();
                    menu.entry(
                        "Checkout",
                        None,
                        window.handler_for(&this, move |this, window, cx| {
                            if let Some(repo) = &this.active_repo {
                                let receiver = repo.update(cx, |repo, _| {
                                    repo.change_branch(name.to_string())
                                });
                                cx.spawn_in(window, async move |this, cx| {
                                    receiver.await.ok();
                                    this.update(cx, |this, cx| this.refresh_branches(cx)).ok();
                                })
                                .detach();
                            }
                        }),
                    )
                } else {
                    menu
                };

                let menu = menu.separator().header("Remote");

                let menu = if !is_remote {
                    let name = branch_name_for_menu.clone();
                    let repo = repo.clone();
                    menu.entry(
                        if has_upstream { "Push" } else { "Push (set upstream)" },
                        None,
                        move |_window, _cx| {
                            // Push handled via git panel's push flow; surface reminder to use it
                            let _ = (&name, &repo);
                        },
                    )
                } else {
                    menu
                };

                let menu = menu.separator().header("Integrate");

                let menu = if !is_head {
                    let name = branch_name_for_menu.clone();
                    let repo_merge = repo.clone();
                    let repo_rebase = repo.clone();
                    let name_rebase = name.clone();
                    menu.entry(
                        "Merge into current branch",
                        None,
                        window.handler_for(&this, move |this, window, cx| {
                            if let Some(repo) = &repo_merge {
                                let receiver = repo.update(cx, |repo, _| {
                                    repo.merge(name.to_string())
                                });
                                cx.spawn_in(window, async move |this, cx| {
                                    match receiver.await {
                                        Ok(Ok(())) => {
                                            this.update(cx, |this, cx| this.refresh_branches(cx)).ok();
                                        }
                                        Ok(Err(error)) => {
                                            log::error!("merge failed: {error}");
                                        }
                                        Err(_) => {}
                                    }
                                })
                                .detach();
                            }
                        }),
                    )
                    .entry(
                        "Rebase current branch onto this",
                        None,
                        window.handler_for(&this, move |this, window, cx| {
                            if let Some(repo) = &repo_rebase {
                                let receiver = repo.update(cx, |repo, _| {
                                    repo.rebase_onto(name_rebase.to_string())
                                });
                                cx.spawn_in(window, async move |this, cx| {
                                    match receiver.await {
                                        Ok(Ok(())) => {
                                            this.update(cx, |this, cx| this.refresh_branches(cx)).ok();
                                        }
                                        Ok(Err(error)) => {
                                            log::error!("rebase failed: {error}");
                                        }
                                        Err(_) => {}
                                    }
                                })
                                .detach();
                            }
                        }),
                    )
                } else {
                    menu
                };

                let menu = menu.separator().header("Manage");

                let name_delete = branch_name_for_menu.clone();
                let repo_delete = repo.clone();
                menu.entry(
                    "Delete Branch",
                    None,
                    window.handler_for(&this, move |this, window, cx| {
                        if let Some(repo) = &repo_delete {
                            let receiver = repo.update(cx, |repo, _| {
                                repo.delete_branch(is_remote, name_delete.to_string(), false)
                            });
                            cx.spawn_in(window, async move |this, cx| {
                                receiver.await.ok();
                                this.update(cx, |this, cx| this.refresh_branches(cx)).ok();
                            })
                            .detach();
                        }
                    }),
                )
            }
        });

        window.focus(&context_menu.focus_handle(cx), cx);
        let subscription = cx.subscribe(&context_menu, |this, _, _: &DismissEvent, cx| {
            this.context_menu = None;
            cx.notify();
        });
        self._subscriptions.push(subscription);
        self.context_menu = Some(BranchContextMenu {
            menu: context_menu,
            _branch_name: branch_name,
        });
        cx.notify();
    }

    fn render_branch_row(
        &self,
        branch: &Branch,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let name = branch.name().to_string();
        let is_head = branch.is_head;
        let icon = if is_head {
            IconName::Check
        } else if branch.is_remote() {
            IconName::Screen
        } else {
            IconName::GitBranch
        };

        let tracking_label = branch.upstream.as_ref().and_then(|upstream| {
            if let UpstreamTracking::Tracked(status) = &upstream.tracking {
                let ahead = status.ahead;
                let behind = status.behind;
                if ahead > 0 || behind > 0 {
                    Some(format!("↑{ahead} ↓{behind}"))
                } else {
                    None
                }
            } else if matches!(upstream.tracking, UpstreamTracking::Gone) {
                Some("gone".to_string())
            } else {
                None
            }
        });

        let last_commit = branch.most_recent_commit.as_ref().map(|c| {
            let ts = OffsetDateTime::from_unix_timestamp(c.commit_timestamp).ok();
            let relative = ts.map(|t| {
                let now = OffsetDateTime::now_utc();
                let diff = now - t;
                if diff < Duration::hours(1) {
                    format!("{}m ago", diff.whole_minutes().max(0))
                } else if diff < Duration::days(1) {
                    format!("{}h ago", diff.whole_hours())
                } else {
                    format!("{}d ago", diff.whole_days())
                }
            });
            (c.subject.clone(), relative.unwrap_or_default())
        });

        let branch_for_menu = branch.clone();
        let this = cx.entity();

        h_flex()
            .gap_1()
            .px_2()
            .py_1()
            .w_full()
            .cursor_pointer()
            .rounded_md()
            .hover(|s| s.bg(cx.theme().colors().element_hover))
            .on_mouse_down(
                gpui::MouseButton::Right,
                cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                    this.deploy_branch_context_menu(&branch_for_menu, event.position, window, cx);
                }),
            )
            .child(
                Icon::new(icon)
                    .size(IconSize::Small)
                    .color(if is_head { Color::Accent } else { Color::Muted }),
            )
            .child(
                Label::new(name)
                    .size(LabelSize::Small)
                    .color(if is_head { Color::Default } else { Color::Muted }),
            )
            .when_some(tracking_label, |el, label| {
                el.child(
                    Label::new(label)
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                )
            })
            .when_some(last_commit, |el, (subject, time)| {
                el.ml_auto().child(
                    Label::new(time)
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                )
            })
    }

    fn render_section(
        &self,
        label: &str,
        expanded: bool,
        branches: &[&Branch],
        toggle_action: impl Fn(&mut Window, &mut App) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let rows: Vec<_> = if expanded {
            branches
                .iter()
                .map(|branch| self.render_branch_row(branch, cx).into_any_element())
                .collect()
        } else {
            Vec::new()
        };

        v_flex()
            .w_full()
            .child(
                h_flex()
                    .px_2()
                    .py_1()
                    .w_full()
                    .cursor_pointer()
                    .gap_1()
                    .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| toggle_action(window, cx))
                    .child(
                        Icon::new(if expanded {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .size(IconSize::Small)
                        .color(Color::Muted),
                    )
                    .child(
                        Label::new(label.to_string())
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    ),
            )
            .children(rows)
    }
}

impl EventEmitter<PanelEvent> for BranchPanel {}

impl Focusable for BranchPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for BranchPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let local_branches: Vec<&Branch> = self
            .branches
            .iter()
            .filter(|b| !b.is_remote())
            .collect();
        let remote_branches: Vec<&Branch> = self
            .branches
            .iter()
            .filter(|b| b.is_remote())
            .collect();

        let this = cx.entity();

        v_flex()
            .key_context("BranchPanel")
            .track_focus(&self.focus_handle)
            .size_full()
            .child(
                h_flex()
                    .px_2()
                    .py_1()
                    .border_b_1()
                    .border_color(cx.theme().colors().border)
                    .child(
                        Label::new("Branches")
                            .size(LabelSize::Small)
                            .color(Color::Default),
                    )
                    .ml_auto()
                    .child(
                        ui::IconButton::new("refresh-branches", IconName::ArrowCircle)
                            .icon_size(IconSize::Small)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.refresh_branches(cx);
                            }))
                            .tooltip(Tooltip::text("Refresh branches")),
                    ),
            )
            .when(self.is_loading, |el| {
                el.child(
                    h_flex()
                        .p_2()
                        .child(Label::new("Loading…").size(LabelSize::Small).color(Color::Muted)),
                )
            })
            .when_some(self.error.clone(), |el, error| {
                el.child(
                    h_flex().p_2().child(
                        Label::new(error)
                            .size(LabelSize::Small)
                            .color(Color::Error),
                    ),
                )
            })
            .when(!self.is_loading, |el| {
                el.child(
                    v_flex()
                        .overflow_hidden()
                        .size_full()
                        .child(self.render_section(
                            &format!("Local ({})", local_branches.len()),
                            self.local_expanded,
                            &local_branches,
                            {
                                let this = this.clone();
                                move |_, cx| {
                                    this.update(cx, |this, cx| {
                                        this.local_expanded = !this.local_expanded;
                                        cx.notify();
                                    });
                                }
                            },
                            cx,
                        ))
                        .child(Divider::horizontal())
                        .child(self.render_section(
                            &format!("Remote ({})", remote_branches.len()),
                            self.remote_expanded,
                            &remote_branches,
                            {
                                let this = this.clone();
                                move |_, cx| {
                                    this.update(cx, |this, cx| {
                                        this.remote_expanded = !this.remote_expanded;
                                        cx.notify();
                                    });
                                }
                            },
                            cx,
                        )),
                )
            })
            .when_some(self.context_menu.as_ref(), |el, menu| {
                el.child(gpui::deferred(
                    gpui::anchored().child(menu.menu.clone()),
                ))
            })
    }
}

impl Panel for BranchPanel {
    fn persistent_name() -> &'static str {
        "BranchPanel"
    }

    fn panel_key() -> &'static str {
        BRANCH_PANEL_KEY
    }

    fn position(&self, _: &Window, _cx: &App) -> DockPosition {
        DockPosition::Left
    }

    fn position_is_valid(&self, position: DockPosition) -> bool {
        matches!(
            position,
            DockPosition::Left | DockPosition::Right
        )
    }

    fn set_position(
        &mut self,
        _position: DockPosition,
        _: &mut Window,
        _cx: &mut Context<Self>,
    ) {
    }

    fn default_size(&self, _: &Window, _cx: &App) -> gpui::Pixels {
        gpui::px(240.)
    }

    fn icon(&self, _: &Window, _cx: &App) -> Option<ui::IconName> {
        Some(ui::IconName::GitBranch)
    }

    fn icon_tooltip(&self, _: &Window, _cx: &App) -> Option<&'static str> {
        Some("Branch Panel")
    }

    fn toggle_action(&self) -> Box<dyn Action> {
        Box::new(ToggleFocus)
    }

    fn activation_priority(&self) -> u32 {
        2
    }
}
