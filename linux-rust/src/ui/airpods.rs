use crate::bluetooth::aacp::{AACPManager, ControlCommandIdentifiers};
use crate::bluetooth::heart_rate::{HISTORY_LEN, HeartRateStats};
use iced::Alignment::End;
use iced::border::Radius;
use iced::overlay::menu;
use iced::widget::button::Style;
use iced::widget::rule::FillMode;
use iced::widget::{
    Row, Space, button, column, combo_box, container, row, rule, scrollable, text, text_input,
    toggler,
};
use iced::{Background, Border, Bottom, Center, Color, Length, Padding, Theme};
use log::error;
use std::collections::HashMap;
use std::sync::Arc;
use std::thread;
use tokio::runtime::Runtime;
// use crate::bluetooth::att::ATTManager;
use crate::devices::enums::{AirPodsState, DeviceData, DeviceInformation, DeviceState};
use crate::ui::window::Message;

pub fn airpods_view<'a>(
    mac: &'a str,
    devices_list: &HashMap<String, DeviceData>,
    state: &'a AirPodsState,
    aacp_manager: Arc<AACPManager>,
    // att_manager: Arc<ATTManager>
) -> iced::widget::Container<'a, Message> {
    let mac = mac.to_string();
    // order: name, noise control, press and hold config, call controls (not sure if why it might be needed, adding it just in case), audio (personalized volume, conversational awareness, adaptive audio slider), connection settings, microphone, head gestures (not adding this), off listening mode, device information

    let aacp_manager_for_rename = aacp_manager.clone();
    let rename_input = container(
        row![
            Space::new().width(10),
            text("Name").size(16).style(|theme: &Theme| {
                let mut style = text::Style::default();
                style.color = Some(theme.palette().text);
                style
            }),
            Space::new().width(Length::Fill),
            text_input("", &state.device_name)
                .padding(Padding {
                    top: 5.0,
                    bottom: 5.0,
                    left: 10.0,
                    right: 10.0,
                })
                .style(|theme: &Theme, _status| {
                    text_input::Style {
                        background: Background::Color(Color::TRANSPARENT),
                        border: Default::default(),
                        icon: Default::default(),
                        placeholder: theme.palette().text.scale_alpha(0.7),
                        value: theme.palette().text,
                        selection: Default::default(),
                    }
                })
                .align_x(End)
                .on_input({
                    let mac = mac.clone();
                    let state = state.clone();
                    move |new_name| {
                        let aacp_manager = aacp_manager_for_rename.clone();
                        run_async_in_thread({
                            let new_name = new_name.clone();
                            async move {
                                aacp_manager
                                    .send_rename_packet(&new_name)
                                    .await
                                    .expect("Failed to send rename packet");
                            }
                        });
                        let mut state = state.clone();
                        state.device_name = new_name.clone();
                        Message::StateChanged(mac.to_string(), DeviceState::AirPods(state))
                    }
                })
        ]
        .align_y(Center),
    )
    .padding(Padding {
        top: 5.0,
        bottom: 5.0,
        left: 10.0,
        right: 10.0,
    })
    .style(|theme: &Theme| {
        let mut style = container::Style::default();
        style.background = Some(Background::Color(theme.palette().primary.scale_alpha(0.1)));
        let mut border = Border::default();
        border.color = theme.palette().primary.scale_alpha(0.5);
        style.border = border.rounded(16);
        style
    });

    let listening_mode = container(
        row![
            text("Listening Mode").size(16).style(|theme: &Theme| {
                let mut style = text::Style::default();
                style.color = Some(theme.palette().text);
                style
            }),
            Space::new().width(Length::Fill),
            {
                let state_clone = state.clone();
                let mac = mac.clone();
                // this combo_box doesn't go really well with the design, but I am not writing my own dropdown menu for this
                combo_box(
                    &state.noise_control_state,
                    "Select Listening Mode",
                    Some(&state.noise_control_mode.clone()),
                    {
                        let aacp_manager = aacp_manager.clone();
                        move |selected_mode| {
                            let aacp_manager = aacp_manager.clone();
                            let selected_mode_c = selected_mode.clone();
                            run_async_in_thread(async move {
                                aacp_manager
                                    .send_control_command(
                                        ControlCommandIdentifiers::ListeningMode,
                                        &[selected_mode_c.to_byte()],
                                    )
                                    .await
                                    .expect("Failed to send Noise Control Mode command");
                            });
                            let mut state = state_clone.clone();
                            state.noise_control_mode = selected_mode.clone();
                            Message::StateChanged(mac.to_string(), DeviceState::AirPods(state))
                        }
                    },
                )
                .width(Length::from(200))
                .input_style(|theme: &Theme, _status| text_input::Style {
                    background: Background::Color(theme.palette().primary.scale_alpha(0.2)),
                    border: Border {
                        width: 1.0,
                        color: theme.palette().text.scale_alpha(0.3),
                        radius: Radius::from(4.0),
                    },
                    icon: Default::default(),
                    placeholder: theme.palette().text,
                    value: theme.palette().text,
                    selection: Default::default(),
                })
                .padding(Padding {
                    top: 5.0,
                    bottom: 5.0,
                    left: 10.0,
                    right: 10.0,
                })
                .menu_style(|theme: &Theme| menu::Style {
                    background: Background::Color(theme.palette().background),
                    border: Border {
                        width: 1.0,
                        color: theme.palette().text,
                        radius: Radius::from(4.0),
                    },
                    text_color: theme.palette().text,
                    selected_text_color: theme.palette().text,
                    selected_background: Background::Color(
                        theme.palette().primary.scale_alpha(0.3),
                    ),
                    shadow: Default::default()
                })
            }
        ]
        .align_y(Center),
    )
    .padding(Padding {
        top: 5.0,
        bottom: 5.0,
        left: 18.0,
        right: 18.0,
    })
    .style(|theme: &Theme| {
        let mut style = container::Style::default();
        style.background = Some(Background::Color(theme.palette().primary.scale_alpha(0.1)));
        let mut border = Border::default();
        border.color = theme.palette().primary.scale_alpha(0.5);
        style.border = border.rounded(16);
        style
    });

    let mac_audio = mac.clone();
    let mac_information = mac.clone();

    let audio_settings_col = column![
        container(
            text("Audio Settings").size(18).style(
                |theme: &Theme| {
                    let mut style = text::Style::default();
                    style.color = Some(theme.palette().primary);
                    style
                }
            )
        )
        .padding(Padding{
            top: 5.0,
            bottom: 5.0,
            left: 18.0,
            right: 18.0,
        }),

        container(
            column![
                {
                    let aacp_manager_pv = aacp_manager.clone();
                    row![
                        column![
                            text("Personalized Volume").size(16),
                            text("Adjusts the volume in response to your environment.").size(12).style(
                                |theme: &Theme| {
                                    let mut style = text::Style::default();
                                    style.color = Some(theme.palette().text.scale_alpha(0.7));
                                    style
                                }
                            ).width(Length::Fill),
                        ].width(Length::Fill),
                        toggler(state.personalized_volume_enabled)
                            .on_toggle(
                            {
                                let mac = mac_audio.clone();
                                let state = state.clone();
                                move |is_enabled| {
                                    let aacp_manager = aacp_manager_pv.clone();
                                    let mac = mac.clone();
                                    run_async_in_thread(
                                        async move {
                                            aacp_manager.send_control_command(
                                                ControlCommandIdentifiers::AdaptiveVolumeConfig,
                                                if is_enabled { &[0x01] } else { &[0x02] }
                                            ).await.expect("Failed to send Personalized Volume command");
                                        }
                                    );
                                    let mut state = state.clone();
                                    state.personalized_volume_enabled = is_enabled;
                                    Message::StateChanged(mac, DeviceState::AirPods(state))
                                }
                            }
                        )
                        .spacing(0)
                        .size(20)
                    ]
                    .align_y(Center)
                    .spacing(8)
                },
                rule::horizontal(1).style(
                    |theme: &Theme| {
                        rule::Style {
                            color: theme.palette().text.scale_alpha(0.2),
                            radius: Radius::from(12),
                            fill_mode: FillMode::Full,
                            snap: false
                        }
                    }
                ),
                {
                    let aacp_manager_conv_detect = aacp_manager.clone();
                    row![
                        column![
                            text("Conversation Awareness").size(16),
                            text("Lowers the volume of your audio when it detects that you are speaking.").size(12).style(
                                |theme: &Theme| {
                                    let mut style = text::Style::default();
                                    style.color = Some(theme.palette().text.scale_alpha(0.7));
                                    style
                                }
                            ).width(Length::Fill),
                        ].width(Length::Fill),
                        toggler(state.conversation_awareness_enabled)
                            .on_toggle(move |is_enabled| {
                                let aacp_manager = aacp_manager_conv_detect.clone();
                                run_async_in_thread(
                                    async move {
                                        aacp_manager.send_control_command(
                                            ControlCommandIdentifiers::ConversationDetectConfig,
                                            if is_enabled { &[0x01] } else { &[0x02] }
                                        ).await.expect("Failed to send Conversation Awareness command");
                                    }
                                );
                                let mut state = state.clone();
                                state.conversation_awareness_enabled = is_enabled;
                                Message::StateChanged(mac_audio.to_string(), DeviceState::AirPods(state))
                            })
                        .spacing(0)
                        .size(20)
                    ]
                    .align_y(Center)
                    .spacing(8)
                }
            ]
                .spacing(4)
                .padding(8)
        )
        .padding(Padding{
            top: 5.0,
            bottom: 5.0,
            left: 10.0,
            right: 10.0,
        })
        .style(
            |theme: &Theme| {
                let mut style = container::Style::default();
                style.background = Some(Background::Color(theme.palette().primary.scale_alpha(0.1)));
                let mut border = Border::default();
                border.color = theme.palette().primary.scale_alpha(0.5);
                style.border = border.rounded(16);
                style
            }
        )
    ];

    let off_listening_mode_toggle = {
        let aacp_manager_olm = aacp_manager.clone();
        let mac = mac.clone();
        container(row![
            column![
                text("Off Listening Mode").size(16),
                text("When this is on, AirPods listening modes will include an Off option. Loud sound levels are not reduced when listening mode is set to Off.").size(12).style(
                    |theme: &Theme| {
                        let mut style = text::Style::default();
                        style.color = Some(theme.palette().text.scale_alpha(0.7));
                        style
                    }
                ).width(Length::Fill)
            ].width(Length::Fill),
            toggler(state.allow_off_mode)
                .on_toggle(move |is_enabled| {
                    let aacp_manager = aacp_manager_olm.clone();
                    run_async_in_thread(
                        async move {
                            aacp_manager.send_control_command(
                                ControlCommandIdentifiers::AllowOffOption,
                                if is_enabled { &[0x01] } else { &[0x02] }
                            ).await.expect("Failed to send Off Listening Mode command");
                        }
                    );
                    let mut state = state.clone();
                    state.allow_off_mode = is_enabled;
                    Message::StateChanged(mac.to_string(), DeviceState::AirPods(state))
                })
            .spacing(0)
            .size(20)
        ]
            .align_y(Center)
            .spacing(8)
        )
            .padding(Padding{
                top: 5.0,
                bottom: 5.0,
                left: 18.0,
                right: 18.0,
            })
            .style(
                |theme: &Theme| {
                    let mut style = container::Style::default();
                    style.background = Some(Background::Color(theme.palette().primary.scale_alpha(0.1)));
                    let mut border = Border::default();
                    border.color = theme.palette().primary.scale_alpha(0.5);
                    style.border = border.rounded(16);
                    style
                }
            )
    };

    let heart_rate_col = heart_rate_section(&mac, state, aacp_manager.clone());

    let mut information_col = column![];
    if let Some(device) = devices_list.get(mac_information.as_str()) {
        if let Some(DeviceInformation::AirPods(ref airpods_info)) = device.information {
            let info_rows = column![
                row![
                    text("Model Number").size(16).style(|theme: &Theme| {
                        let mut style = text::Style::default();
                        style.color = Some(theme.palette().text);
                        style
                    }),
                    Space::new().width(Length::Fill),
                    text(airpods_info.model_number.clone()).size(16)
                ],
                row![
                    text("Manufacturer").size(16).style(|theme: &Theme| {
                        let mut style = text::Style::default();
                        style.color = Some(theme.palette().text);
                        style
                    }),
                    Space::new().width(Length::Fill),
                    text(airpods_info.manufacturer.clone()).size(16)
                ],
                row![
                    text("Serial Number").size(16).style(|theme: &Theme| {
                        let mut style = text::Style::default();
                        style.color = Some(theme.palette().text);
                        style
                    }),
                    Space::new().width(Length::Fill),
                    button(text(airpods_info.serial_number.clone()).size(16))
                        .style(|theme: &Theme, _status| {
                            let mut style = Style::default();
                            style.text_color = theme.palette().text;
                            style.background = Some(Background::Color(Color::TRANSPARENT));
                            style
                        })
                        .padding(0)
                        .on_press(Message::CopyToClipboard(airpods_info.serial_number.clone()))
                ],
                row![
                    text("Left Serial Number").size(16).style(|theme: &Theme| {
                        let mut style = text::Style::default();
                        style.color = Some(theme.palette().text);
                        style
                    }),
                    Space::new().width(Length::Fill),
                    button(text(airpods_info.left_serial_number.clone()).size(16))
                        .style(|theme: &Theme, _status| {
                            let mut style = Style::default();
                            style.text_color = theme.palette().text;
                            style.background = Some(Background::Color(Color::TRANSPARENT));
                            style
                        })
                        .padding(0)
                        .on_press(Message::CopyToClipboard(
                            airpods_info.left_serial_number.clone()
                        ))
                ],
                row![
                    text("Right Serial Number").size(16).style(|theme: &Theme| {
                        let mut style = text::Style::default();
                        style.color = Some(theme.palette().text);
                        style
                    }),
                    Space::new().width(Length::Fill),
                    button(text(airpods_info.right_serial_number.clone()).size(16))
                        .style(|theme: &Theme, _status| {
                            let mut style = Style::default();
                            style.text_color = theme.palette().text;
                            style.background = Some(Background::Color(Color::TRANSPARENT));
                            style
                        })
                        .padding(0)
                        .on_press(Message::CopyToClipboard(
                            airpods_info.right_serial_number.clone()
                        ))
                ],
                row![
                    text("Version 1").size(16).style(|theme: &Theme| {
                        let mut style = text::Style::default();
                        style.color = Some(theme.palette().text);
                        style
                    }),
                    Space::new().width(Length::Fill),
                    text(airpods_info.version1.clone()).size(16)
                ],
                row![
                    text("Version 2").size(16).style(|theme: &Theme| {
                        let mut style = text::Style::default();
                        style.color = Some(theme.palette().text);
                        style
                    }),
                    Space::new().width(Length::Fill),
                    text(airpods_info.version2.clone()).size(16)
                ],
                row![
                    text("Version 3").size(16).style(|theme: &Theme| {
                        let mut style = text::Style::default();
                        style.color = Some(theme.palette().text);
                        style
                    }),
                    Space::new().width(Length::Fill),
                    text(airpods_info.version3.clone()).size(16)
                ]
            ]
            .spacing(4)
            .padding(8);

            information_col = column![
                container(text("Device Information").size(18).style(|theme: &Theme| {
                    let mut style = text::Style::default();
                    style.color = Some(theme.palette().primary);
                    style
                }))
                .padding(Padding {
                    top: 5.0,
                    bottom: 5.0,
                    left: 18.0,
                    right: 18.0,
                }),
                container(info_rows)
                    .padding(Padding {
                        top: 5.0,
                        bottom: 5.0,
                        left: 10.0,
                        right: 10.0,
                    })
                    .style(|theme: &Theme| {
                        let mut style = container::Style::default();
                        style.background =
                            Some(Background::Color(theme.palette().primary.scale_alpha(0.1)));
                        let mut border = Border::default();
                        border.color = theme.palette().primary.scale_alpha(0.5);
                        style.border = border.rounded(16);
                        style
                    })
            ];
        } else {
            error!(
                "Expected AirPodsInformation for device {}, got something else",
                mac.clone()
            );
        }
    }

    container(scrollable(column![
        rename_input,
        Space::new().height(Length::from(20)),
        listening_mode,
        Space::new().height(Length::from(20)),
        audio_settings_col,
        Space::new().height(Length::from(20)),
        heart_rate_col,
        Space::new().height(Length::from(20)),
        off_listening_mode_toggle,
        Space::new().height(Length::from(20)),
        information_col
    ].padding(Padding {
        top: 0.0,
        bottom: 0.0,
        left: 0.0,
        right: 12.0,
    })))
    .padding(20)
    .center_x(Length::Fill)
    .height(Length::Fill)
}

fn heart_rate_section<'a>(
    mac: &str,
    state: &'a AirPodsState,
    aacp_manager: Arc<AACPManager>,
) -> iced::widget::Column<'a, Message> {
    let stats = &state.heart_rate;
    let secondary_text = |theme: &Theme| {
        let mut style = text::Style::default();
        style.color = Some(theme.palette().text.scale_alpha(0.7));
        style
    };
    let separator = || {
        rule::horizontal(1).style(|theme: &Theme| rule::Style {
            color: theme.palette().text.scale_alpha(0.2),
            radius: Radius::from(12),
            fill_mode: FillMode::Full,
            snap: false,
        })
    };
    let stat_row = |label: &'static str, value: String| {
        row![
            text(label).size(16),
            Space::new().width(Length::Fill),
            text(value).size(16)
        ]
    };
    let bpm_text = |bpm: Option<f64>| {
        bpm.map(|b| format!("{:.0} bpm", b))
            .unwrap_or_else(|| "-".to_string())
    };

    let toggle = {
        let aacp_manager = aacp_manager.clone();
        let mac = mac.to_string();
        let state = state.clone();
        row![
            column![
                text("Heart Rate Monitoring").size(16),
                text("Streams heart rate readings from supported AirPods (e.g. AirPods Pro 3). Wear both AirPods for accurate readings.")
                    .size(12)
                    .style(secondary_text)
                    .width(Length::Fill),
            ]
            .width(Length::Fill),
            toggler(stats.monitoring)
                .on_toggle(move |is_enabled| {
                    let aacp_manager = aacp_manager.clone();
                    run_async_in_thread(async move {
                        let result = if is_enabled {
                            aacp_manager.start_heart_rate_monitoring().await
                        } else {
                            aacp_manager.stop_heart_rate_monitoring().await
                        };
                        if let Err(e) = result {
                            error!("Failed to toggle heart rate monitoring: {}", e);
                        }
                    });
                    let mut state = state.clone();
                    state.heart_rate.monitoring = is_enabled;
                    Message::StateChanged(mac.clone(), DeviceState::AirPods(state))
                })
                .spacing(0)
                .size(20)
        ]
        .align_y(Center)
        .spacing(8)
    };

    let current = row![
        text(
            stats
                .current()
                .map(|b| b.to_string())
                .unwrap_or_else(|| "--".to_string())
        )
        .size(40)
        .style(|theme: &Theme| {
            let mut style = text::Style::default();
            style.color = Some(theme.palette().danger);
            style
        }),
        column![
            text("BPM").size(14).style(secondary_text),
            text(match stats.trend(10) {
                Some(d) if d >= 3 => format!("↑ {:+}", d),
                Some(d) if d <= -3 => format!("↓ {:+}", d),
                Some(d) => format!("→ {:+}", d),
                None => String::new(),
            })
            .size(12)
            .style(secondary_text),
        ],
        Space::new().width(Length::Fill),
        text(if stats.monitoring {
            if stats.count == 0 {
                "Waiting for readings…"
            } else {
                "Live"
            }
        } else {
            "Stopped"
        })
        .size(12)
        .style(secondary_text),
    ]
    .align_y(Center)
    .spacing(8);

    let duration = stats
        .duration()
        .map(|d| {
            let secs = d.as_secs();
            format!("{}:{:02}:{:02}", secs / 3600, (secs / 60) % 60, secs % 60)
        })
        .unwrap_or_else(|| "-".to_string());

    let reset_button = {
        let aacp_manager = aacp_manager.clone();
        let mac = mac.to_string();
        let state = state.clone();
        button(text("Reset Statistics").size(14))
            .style(|theme: &Theme, _status| {
                let mut style = Style::default();
                style.text_color = theme.palette().primary;
                style.background = Some(Background::Color(Color::TRANSPARENT));
                style
            })
            .padding(0)
            .on_press_with(move || {
                let aacp_manager = aacp_manager.clone();
                run_async_in_thread(async move {
                    aacp_manager.reset_heart_rate_stats().await;
                });
                let mut state = state.clone();
                state.heart_rate.reset();
                Message::StateChanged(mac.clone(), DeviceState::AirPods(state))
            })
    };

    let mut rows = column![toggle, separator(), current]
        .spacing(4)
        .padding(8);
    if let Some(graph) = heart_rate_graph(stats) {
        rows = rows.push(graph);
    }
    rows = rows.push(separator()).extend([
        stat_row("Average", bpm_text(stats.average())).into(),
        stat_row("Last 30 Readings", bpm_text(stats.recent_average(30))).into(),
        stat_row("Minimum", bpm_text(stats.min.map(f64::from))).into(),
        stat_row("Maximum", bpm_text(stats.max.map(f64::from))).into(),
        stat_row("Readings", stats.count.to_string()).into(),
        stat_row("Duration", duration).into(),
        row![Space::new().width(Length::Fill), reset_button].into(),
    ]);

    column![
        container(text("Heart Rate").size(18).style(|theme: &Theme| {
            let mut style = text::Style::default();
            style.color = Some(theme.palette().primary);
            style
        }))
        .padding(Padding {
            top: 5.0,
            bottom: 5.0,
            left: 18.0,
            right: 18.0,
        }),
        container(rows)
            .padding(Padding {
                top: 5.0,
                bottom: 5.0,
                left: 10.0,
                right: 10.0,
            })
            .style(|theme: &Theme| {
                let mut style = container::Style::default();
                style.background =
                    Some(Background::Color(theme.palette().primary.scale_alpha(0.1)));
                let mut border = Border::default();
                border.color = theme.palette().primary.scale_alpha(0.5);
                style.border = border.rounded(16);
                style
            })
    ]
}

/// Simple bar graph of the recent heart rate history.
fn heart_rate_graph<'a>(stats: &HeartRateStats) -> Option<iced::widget::Container<'a, Message>> {
    const GRAPH_HEIGHT: f32 = 60.0;
    let lo = stats.history.iter().map(|s| s.bpm).min()?.saturating_sub(5) as f32;
    let hi = stats.history.iter().map(|s| s.bpm).max()?.saturating_add(5) as f32;
    let bars = stats.history.iter().map(|sample| {
        let height = ((sample.bpm as f32 - lo) / (hi - lo)).clamp(0.05, 1.0) * GRAPH_HEIGHT;
        container(Space::new())
            .width(Length::Fill)
            .height(Length::Fixed(height))
            .style(|theme: &Theme| {
                let mut style = container::Style::default();
                style.background = Some(Background::Color(theme.palette().danger.scale_alpha(0.7)));
                style.border = Border::default().rounded(1);
                style
            })
            .into()
    });
    let padding = HISTORY_LEN.saturating_sub(stats.history.len());
    let graph = Row::with_children(
        std::iter::repeat_with(|| Space::new().width(Length::Fill).into())
            .take(padding)
            .chain(bars),
    )
    .spacing(1)
    .height(Length::Fixed(GRAPH_HEIGHT))
    .align_y(Bottom);
    Some(container(graph).padding(Padding {
        top: 4.0,
        bottom: 4.0,
        left: 0.0,
        right: 0.0,
    }))
}

fn run_async_in_thread<F>(fut: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    thread::spawn(move || {
        let rt = Runtime::new().unwrap();
        rt.block_on(fut);
    });
}
