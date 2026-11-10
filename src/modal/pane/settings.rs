use crate::chart::comparison::ComparisonChart;
use crate::screen::dashboard::pane::{Event, Message};
use crate::screen::dashboard::panel::timeandsales;
use crate::split_column;
use crate::widget::{classic_slider_row, labeled_slider};
use crate::{style, tooltip, widget::scrollable_content};

use data::chart::heatmap::HeatmapStudy;
use data::chart::kline::FootprintStudy;
use data::chart::{
    KlineChartKind,
    bookmap::{self, BubbleScale, Colormap},
    heatmap::{self},
    kline::ClusterKind,
};
use data::layout::pane::VisualConfig;
use data::panel::ladder;
use data::panel::timeandsales::{StackedBar, StackedBarRatio};
use data::util::format_with_commas;

use iced::widget::{checkbox, space};
use iced::{
    Alignment, Element, Length,
    widget::{
        button, column, container, pane_grid, pick_list, radio, row, slider, text,
        tooltip::Position as TooltipPosition,
    },
};
use std::time::Duration;

fn cfg_view_container<'a, T>(max_width: u32, content: T) -> Element<'a, Message>
where
    T: Into<Element<'a, Message>>,
{
    container(scrollable_content(content))
        .width(Length::Shrink)
        .padding(28)
        .max_width(max_width)
        .style(style::chart_modal)
        .into()
}

pub fn bookmap_cfg_view<'a>(
    cfg: bookmap::Config,
    heatmap_cfg: heatmap::Config,
    pane: pane_grid::Pane,
    study_config: &'a study::Configurator<HeatmapStudy>,
    studies: &'a [HeatmapStudy],
    basis: data::chart::Basis,
) -> Element<'a, Message> {
    let intensity_slider = classic_slider_row(
        text("Intensity"),
        slider(0.1..=3.0, cfg.heatmap_intensity, move |value| {
            Message::VisualConfigChanged(
                pane,
                VisualConfig::Bookmap(bookmap::Config {
                    heatmap_intensity: value,
                    ..cfg
                }),
                false,
            )
        })
        .step(0.05)
        .into(),
        Some(text(format!("{:.2}", cfg.heatmap_intensity)).size(crate::style::text_size::EMPHASIS)),
    );

    let gamma_slider = classic_slider_row(
        text("Gamma"),
        slider(0.1..=2.0, cfg.heatmap_gamma, move |value| {
            Message::VisualConfigChanged(
                pane,
                VisualConfig::Bookmap(bookmap::Config {
                    heatmap_gamma: value,
                    ..cfg
                }),
                false,
            )
        })
        .step(0.05)
        .into(),
        Some(text(format!("{:.2}", cfg.heatmap_gamma)).size(crate::style::text_size::EMPHASIS)),
    );

    let min_qty_slider = labeled_slider(
        "Min qty",
        0.0..=1_000.0,
        cfg.heatmap_min_qty,
        move |value| {
            Message::VisualConfigChanged(
                pane,
                VisualConfig::Bookmap(bookmap::Config {
                    heatmap_min_qty: value,
                    ..cfg
                }),
                false,
            )
        },
        |value| format_with_commas(*value as f64),
        Some(1.0),
    );

    let colormap_row = {
        let options = Colormap::ALL.iter().map(|&cmap| {
            radio(
                cmap.to_string(),
                cmap,
                Some(cfg.heatmap_colormap),
                move |value| {
                    Message::VisualConfigChanged(
                        pane,
                        VisualConfig::Bookmap(bookmap::Config {
                            heatmap_colormap: value,
                            ..cfg
                        }),
                        false,
                    )
                },
            )
            .spacing(4)
            .into()
        });

        row![text("Colormap: "), row(options).spacing(12)].spacing(12)
    };

    let heatmap_column = column![
        text("Liquidity heatmap").size(crate::style::text_size::SECTION),
        column![intensity_slider, gamma_slider, min_qty_slider].spacing(8),
        colormap_row,
    ]
    .spacing(8);

    let bubble_size_slider = classic_slider_row(
        text("Bubble size range"),
        row![
            slider(1.0..=20.0, cfg.bubble_min_px, move |value| {
                Message::VisualConfigChanged(
                    pane,
                    VisualConfig::Bookmap(bookmap::Config {
                        bubble_min_px: value.min(cfg.bubble_max_px),
                        ..cfg
                    }),
                    false,
                )
            })
            .step(0.5),
            slider(1.0..=30.0, cfg.bubble_max_px, move |value| {
                Message::VisualConfigChanged(
                    pane,
                    VisualConfig::Bookmap(bookmap::Config {
                        bubble_max_px: value.max(cfg.bubble_min_px),
                        ..cfg
                    }),
                    false,
                )
            })
            .step(0.5),
        ]
        .spacing(8)
        .into(),
        Some(
            text(format!(
                "{:.0}-{:.0}px",
                cfg.bubble_min_px, cfg.bubble_max_px
            ))
            .size(crate::style::text_size::EMPHASIS),
        ),
    );

    let bubble_scale_row = {
        let options = BubbleScale::ALL.iter().map(|&scale| {
            radio(
                scale.to_string(),
                scale,
                Some(cfg.bubble_scale),
                move |value| {
                    Message::VisualConfigChanged(
                        pane,
                        VisualConfig::Bookmap(bookmap::Config {
                            bubble_scale: value,
                            ..cfg
                        }),
                        false,
                    )
                },
            )
            .spacing(4)
            .into()
        });

        row![text("Size scale: "), row(options).spacing(12)].spacing(12)
    };

    let bubble_large_qty_slider = labeled_slider(
        "Large trade",
        0.0..=1_000.0,
        cfg.bubble_large_qty,
        move |value| {
            Message::VisualConfigChanged(
                pane,
                VisualConfig::Bookmap(bookmap::Config {
                    bubble_large_qty: value,
                    ..cfg
                }),
                false,
            )
        },
        |value| format_with_commas(*value as f64),
        Some(1.0),
    );

    let bubble_min_trade_slider = labeled_slider(
        "Min trade",
        0.0..=1_000.0,
        cfg.bubble_min_trade,
        move |value| {
            Message::VisualConfigChanged(
                pane,
                VisualConfig::Bookmap(bookmap::Config {
                    bubble_min_trade: value,
                    ..cfg
                }),
                false,
            )
        },
        |value| format_with_commas(*value as f64),
        Some(1.0),
    );

    let show_bubbles_checkbox = checkbox(cfg.show_bubbles)
        .label("Show trade bubbles")
        .on_toggle(move |value| {
            Message::VisualConfigChanged(
                pane,
                VisualConfig::Bookmap(bookmap::Config {
                    show_bubbles: value,
                    ..cfg
                }),
                false,
            )
        });

    let hollow_small_checkbox = checkbox(cfg.bubble_hollow_small)
        .label("Draw small trades hollow")
        .on_toggle(move |value| {
            Message::VisualConfigChanged(
                pane,
                VisualConfig::Bookmap(bookmap::Config {
                    bubble_hollow_small: value,
                    ..cfg
                }),
                false,
            )
        });

    let bubbles_column = column![
        text("Trade bubbles").size(crate::style::text_size::SECTION),
        show_bubbles_checkbox,
        column![
            bubble_size_slider,
            bubble_scale_row,
            bubble_large_qty_slider,
            bubble_min_trade_slider,
            hollow_small_checkbox,
        ]
        .spacing(8),
    ]
    .spacing(8);

    let show_walls_checkbox = checkbox(cfg.show_walls)
        .label("Show liquidity walls")
        .on_toggle(move |value| {
            Message::VisualConfigChanged(
                pane,
                VisualConfig::Bookmap(bookmap::Config {
                    show_walls: value,
                    ..cfg
                }),
                false,
            )
        });

    let wall_threshold_slider = classic_slider_row(
        text("Wall threshold (x avg)"),
        slider(1.2..=5.0, cfg.wall_threshold_mult, move |value| {
            Message::VisualConfigChanged(
                pane,
                VisualConfig::Bookmap(bookmap::Config {
                    wall_threshold_mult: value,
                    ..cfg
                }),
                false,
            )
        })
        .step(0.1)
        .into(),
        Some(
            text(format!("{:.1}x", cfg.wall_threshold_mult))
                .size(crate::style::text_size::EMPHASIS),
        ),
    );

    let walls_column = column![
        text("Liquidity walls").size(crate::style::text_size::SECTION),
        show_walls_checkbox,
        wall_threshold_slider,
    ]
    .spacing(8);

    let show_bid_ask_lines_checkbox = checkbox(cfg.show_bid_ask_lines)
        .label("Show best bid/ask lines")
        .on_toggle(move |value| {
            Message::VisualConfigChanged(
                pane,
                VisualConfig::Bookmap(bookmap::Config {
                    show_bid_ask_lines: value,
                    ..cfg
                }),
                false,
            )
        });

    let bid_ask_column = column![
        text("Best bid/ask").size(crate::style::text_size::SECTION),
        show_bid_ask_lines_checkbox,
    ]
    .spacing(8);

    let order_size_slider = {
        let filter = heatmap_cfg.order_size_filter;
        labeled_slider(
            "Order",
            0.0..=500_000.0,
            filter,
            move |value| {
                Message::VisualConfigChanged(
                    pane,
                    VisualConfig::Heatmap(heatmap::Config {
                        order_size_filter: value,
                        ..heatmap_cfg
                    }),
                    false,
                )
            },
            |value| format!(">${}", format_with_commas(*value as f64)),
            Some(5000.0),
        )
    };

    let filters_column = column![
        text("Heatmap noise filter").size(crate::style::text_size::SECTION),
        order_size_slider,
    ]
    .spacing(8);

    let study_cfg = study_config.view(studies, basis).map(move |msg| {
        Message::PaneEvent(
            pane,
            Event::StudyConfigurator(study::StudyMessage::Heatmap(msg)),
        )
    });

    let content = split_column![
        heatmap_column,
        bubbles_column,
        walls_column,
        bid_ask_column,
        filters_column,
        column![text("Studies").size(crate::style::text_size::SECTION), study_cfg].spacing(8),
        row![
            space::horizontal(),
            sync_all_button(pane, VisualConfig::Bookmap(cfg))
        ]
        ; spacing = 12, align_x = Alignment::Start
    ];

    cfg_view_container(380, content)
}

pub fn timesales_cfg_view<'a>(
    cfg: timeandsales::Config,
    pane: pane_grid::Pane,
) -> Element<'a, Message> {
    let trade_size_column = {
        let filter = cfg.trade_size_filter;
        let slider = labeled_slider(
            "Trade",
            0.0..=50000.0,
            filter,
            move |value| {
                Message::VisualConfigChanged(
                    pane,
                    VisualConfig::TimeAndSales(timeandsales::Config {
                        trade_size_filter: value,
                        ..cfg
                    }),
                    false,
                )
            },
            |value| format!(">${}", format_with_commas(*value as f64)),
            Some(500.0),
        );

        column![
            text("Size filter").size(crate::style::text_size::SECTION),
            slider
        ]
        .spacing(8)
    };

    let retention_minutes = (cfg.trade_retention.as_secs_f32() / 60.0).max(1.0);
    let retention_slider = {
        let slider_ui = slider(1.0..=180.0, retention_minutes, move |new_minutes| {
            let mins = new_minutes.round().max(1.0) as u64;
            Message::VisualConfigChanged(
                pane,
                VisualConfig::TimeAndSales(timeandsales::Config {
                    trade_retention: Duration::from_secs(mins * 60),
                    ..cfg
                }),
                false,
            )
        })
        .step(1.0);

        classic_slider_row(
            text("Keep trades for"),
            slider_ui.into(),
            Some(
                text(format!("≈ {} min", retention_minutes.round() as u64))
                    .size(crate::style::text_size::EMPHASIS),
            ),
        )
    };

    let history_column = column![
        row![
            text("History").size(crate::style::text_size::SECTION),
            tooltip(
                button("i").style(style::button::info),
                Some("Affects the stacked bar, colors and how much you can scroll down"),
                TooltipPosition::Top,
            )
        ]
        .spacing(4)
        .align_y(Alignment::Center),
        retention_slider
    ]
    .spacing(8);

    let stacked_bar: Element<_> = {
        let is_shown = cfg.stacked_bar.is_some();

        let enable_checkbox = checkbox(is_shown).label("Show stacked bar").on_toggle({
            move |value| {
                let current_ratio = cfg.stacked_bar.map(|h| h.ratio()).unwrap_or_default();
                Message::VisualConfigChanged(
                    pane,
                    VisualConfig::TimeAndSales(timeandsales::Config {
                        stacked_bar: if value {
                            Some(StackedBar::Compact(current_ratio))
                        } else {
                            None
                        },
                        ..cfg
                    }),
                    false,
                )
            }
        });

        let controls: Option<Element<_>> = cfg.stacked_bar.map(|hist| {
            let ratio = hist.ratio();
            let is_compact = matches!(hist, StackedBar::Compact(_));

            let compact = radio("Compact", true, Some(is_compact), {
                move |_v| {
                    Message::VisualConfigChanged(
                        pane,
                        VisualConfig::TimeAndSales(timeandsales::Config {
                            stacked_bar: Some(StackedBar::Compact(ratio)),
                            ..cfg
                        }),
                        false,
                    )
                }
            })
            .spacing(4);

            let full = radio("Full", false, Some(is_compact), {
                move |_v| {
                    Message::VisualConfigChanged(
                        pane,
                        VisualConfig::TimeAndSales(timeandsales::Config {
                            stacked_bar: Some(StackedBar::Full(ratio)),
                            ..cfg
                        }),
                        false,
                    )
                }
            })
            .spacing(4);

            let metric_picklist = pick_list(StackedBarRatio::ALL, Some(ratio), move |new_ratio| {
                let new_hist = Some(match cfg.stacked_bar {
                    Some(StackedBar::Full(_)) => StackedBar::Full(new_ratio),
                    _ => StackedBar::Compact(new_ratio),
                });
                Message::VisualConfigChanged(
                    pane,
                    VisualConfig::TimeAndSales(timeandsales::Config {
                        stacked_bar: new_hist,
                        ..cfg
                    }),
                    false,
                )
            });

            column![
                iced::widget::rule::horizontal(1),
                text("Mode").size(crate::style::text_size::BODY),
                row![compact, full].spacing(12),
                text("Metric").size(crate::style::text_size::BODY),
                metric_picklist,
            ]
            .spacing(8)
            .into()
        });

        let mut inner = column![enable_checkbox]
            .width(Length::Fill)
            .padding(4)
            .spacing(8);

        if let Some(ctrls) = controls {
            inner = inner.push(ctrls);
        }

        container(inner)
            .style(style::modal_container)
            .padding(8)
            .into()
    };

    let content = split_column![
        trade_size_column,
        history_column,
        stacked_bar,
        row![space::horizontal(), sync_all_button(pane, VisualConfig::TimeAndSales(cfg))],
        ; spacing = 12, align_x = Alignment::Start
    ];

    cfg_view_container(320, content)
}

pub fn comparison_cfg_view<'a>(
    pane: pane_grid::Pane,
    chart: &'a ComparisonChart,
) -> Element<'a, Message> {
    let series = &chart.series;
    let series_editor = &chart.series_editor;
    let cfg = chart.config.clone();
    let unit = cfg.unit;

    let pct_radio = {
        let cfg = cfg.clone();
        radio("%", data::chart::comparison::Unit::Percent, Some(unit), {
            move |_v| {
                Message::VisualConfigChanged(
                    pane,
                    VisualConfig::Comparison(data::chart::comparison::Config {
                        unit: data::chart::comparison::Unit::Percent,
                        ..cfg.clone()
                    }),
                    false,
                )
            }
        })
        .spacing(4)
    };

    let price_radio = {
        let cfg = cfg.clone();
        radio("$", data::chart::comparison::Unit::Price, Some(unit), {
            move |_v| {
                Message::VisualConfigChanged(
                    pane,
                    VisualConfig::Comparison(data::chart::comparison::Config {
                        unit: data::chart::comparison::Unit::Price,
                        ..cfg.clone()
                    }),
                    false,
                )
            }
        })
        .spacing(4)
    };

    let unit_row = column![
        text("Unit").size(crate::style::text_size::BODY),
        row![pct_radio, price_radio].spacing(12),
    ]
    .spacing(8);

    let content = column![
        unit_row,
        iced::widget::rule::horizontal(1),
        series_editor.view(series).map(move |msg| {
            Message::PaneEvent(
                pane,
                Event::ComparisonChartInteraction(crate::chart::comparison::Message::Editor(msg)),
            )
        })
    ]
    .spacing(8);

    cfg_view_container(320, content)
}

pub fn kline_cfg_view<'a>(
    study_config: &'a study::Configurator<FootprintStudy>,
    cfg: data::chart::kline::Config,
    kind: &'a KlineChartKind,
    pane: pane_grid::Pane,
    basis: data::chart::Basis,
) -> Element<'a, Message> {
    let display_readout_section = {
        let data_labels_checkbox = tooltip(
            checkbox(cfg.data_labels_always_visible)
                .label("Keep latest label visible")
                .on_toggle(move |value| {
                    Message::VisualConfigChanged(
                        pane,
                        VisualConfig::Kline(data::chart::kline::Config {
                            data_labels_always_visible: value,
                            ..cfg
                        }),
                        false,
                    )
                }),
            Some("Show the latest datapoint label even when not hovering"),
            TooltipPosition::Top,
        );
        column![
            text("Data labels").size(crate::style::text_size::SECTION),
            data_labels_checkbox,
        ]
        .spacing(8)
    };

    let content = match kind {
        KlineChartKind::Candles => {
            let volume_profile_checkbox = tooltip(
                checkbox(cfg.show_volume_profile)
                    .label("Volume profile overlay")
                    .on_toggle(move |value| {
                        Message::VisualConfigChanged(
                            pane,
                            VisualConfig::Kline(data::chart::kline::Config {
                                show_volume_profile: value,
                                ..cfg
                            }),
                            false,
                        )
                    }),
                Some("Show buy/sell volume per price level with candle colors"),
                TooltipPosition::Top,
            );
            let history_slider = labeled_slider(
                "History",
                0u8..=24u8,
                cfg.volume_profile_history_hours,
                move |value| {
                    Message::VisualConfigChanged(
                        pane,
                        VisualConfig::Kline(data::chart::kline::Config {
                            volume_profile_history_hours: value,
                            ..cfg
                        }),
                        false,
                    )
                },
                |v| format!("{v}h"),
                Some(1u8),
            );
            let side_picklist = pick_list(
                data::chart::kline::VolumeProfileSide::ALL,
                Some(cfg.volume_profile_side),
                move |value| {
                    Message::VisualConfigChanged(
                        pane,
                        VisualConfig::Kline(data::chart::kline::Config {
                            volume_profile_side: value,
                            ..cfg
                        }),
                        false,
                    )
                },
            );
            split_column![
                display_readout_section,
                column![text("Volume profile").size(crate::style::text_size::SECTION), volume_profile_checkbox, history_slider, side_picklist].spacing(8),
                row![
                    space::horizontal(),
                    sync_all_button(pane, VisualConfig::Kline(cfg))
                ],
                ; spacing = 12, align_x = Alignment::Start
            ]
        }
        KlineChartKind::Footprint {
            clusters,
            scaling,
            studies,
        } => {
            let cluster_picklist =
                pick_list(ClusterKind::ALL, Some(clusters), move |new_cluster_kind| {
                    Message::PaneEvent(pane, Event::ClusterKindSelected(new_cluster_kind))
                });

            let footprint_summary_checkbox = tooltip(
                checkbox(cfg.show_footprint_summary)
                    .label("Show footprint summary")
                    .on_toggle(move |value| {
                        Message::VisualConfigChanged(
                            pane,
                            VisualConfig::Kline(data::chart::kline::Config {
                                show_footprint_summary: value,
                                ..cfg
                            }),
                            false,
                        )
                    }),
                Some("Show per-bar volume and delta below footprint candles"),
                TooltipPosition::Top,
            );

            let scaling = {
                let picklist = pick_list(
                    data::chart::kline::ClusterScaling::ALL,
                    Some(scaling),
                    move |new_scaling| {
                        Message::PaneEvent(pane, Event::ClusterScalingSelected(new_scaling))
                    },
                );

                if let data::chart::kline::ClusterScaling::Hybrid { weight } = scaling {
                    let hybrid_slider = slider(0.0..=1.0, *weight, move |new_weight| {
                        Message::PaneEvent(
                            pane,
                            Event::ClusterScalingSelected(
                                data::chart::kline::ClusterScaling::Hybrid { weight: new_weight },
                            ),
                        )
                    })
                    .step(0.05);

                    column![
                        picklist,
                        hybrid_slider,
                        text("Blend visible-range and per-candle scaling"),
                    ]
                    .spacing(8)
                } else {
                    column![picklist].spacing(8)
                }
            };

            let available_studies: Vec<_> = data::chart::kline::FootprintStudy::ALL.to_vec();

            let active_studies: Vec<_> = studies
                .iter()
                .copied()
                .filter(|study| {
                    available_studies
                        .iter()
                        .any(|available| available.is_same_type(study))
                })
                .collect();

            let study_cfg = study_config
                .view_available(&active_studies, basis, available_studies)
                .map(move |msg| {
                    Message::PaneEvent(
                        pane,
                        Event::StudyConfigurator(study::StudyMessage::Footprint(msg)),
                    )
                });

            let mut content = split_column![
                display_readout_section,
                column![text("Footprint summary").size(crate::style::text_size::SECTION), footprint_summary_checkbox].spacing(8),
                column![text("Cluster type").size(crate::style::text_size::SECTION), cluster_picklist].spacing(8),
                ; spacing = 12, align_x = Alignment::Start
            ];

            content = content.push(
                column![
                    text("Cluster scaling").size(crate::style::text_size::SECTION),
                    scaling
                ]
                .spacing(8),
            );

            content = content
                .push(
                    column![
                        text("Studies").size(crate::style::text_size::SECTION),
                        study_cfg
                    ]
                    .spacing(8),
                )
                .push(row![
                    space::horizontal(),
                    sync_all_button(pane, VisualConfig::Kline(cfg))
                ]);

            content
        }
    };

    cfg_view_container(360, content)
}

pub fn ladder_cfg_view<'a>(cfg: ladder::Config, pane: pane_grid::Pane) -> Element<'a, Message> {
    let display_options = {
        let invert_layout = checkbox(cfg.invert_layout)
            .label("Invert layout")
            .on_toggle(move |value| {
                Message::VisualConfigChanged(
                    pane,
                    VisualConfig::Ladder(ladder::Config {
                        invert_layout: value,
                        ..cfg
                    }),
                    false,
                )
            });

        let chase_tracker = checkbox(cfg.show_chase_tracker)
            .label("Show Chase Tracker")
            .on_toggle(move |value| {
                Message::VisualConfigChanged(
                    pane,
                    VisualConfig::Ladder(ladder::Config {
                        show_chase_tracker: value,
                        ..cfg
                    }),
                    false,
                )
            });

        column![
            text("Display Options").size(crate::style::text_size::SECTION),
            column![
                row![
                    invert_layout,
                    tooltip(
                        button("i").style(style::button::info),
                        Some("Swaps Bid/Ask Qty with Sell/Buy: the traded sizes move to the outside, the book sits next to the price."),
                        TooltipPosition::Top,
                    )
                ]
                .align_y(Alignment::Center)
                .spacing(4),
                row![
                    chase_tracker,
                    tooltip(
                        button("i").style(style::button::info),
                        Some("Highlights consecutive best-price moves and fades when momentum stalls.\nCalculated using raw ungrouped data."),
                        TooltipPosition::Top,
                    )
                ]
                .align_y(Alignment::Center)
                .spacing(4)
            ]
            .spacing(4)
        ]
        .spacing(8)
    };

    let retention_slider = {
        let retention_minutes = (cfg.trade_retention.as_secs_f32() / 60.0).max(1.0);

        let slider_ui = slider(1.0..=180.0, retention_minutes, move |new_minutes| {
            let mins = new_minutes.round().max(1.0) as u64;
            Message::VisualConfigChanged(
                pane,
                VisualConfig::Ladder(ladder::Config {
                    trade_retention: Duration::from_secs(mins * 60),
                    ..cfg
                }),
                false,
            )
        })
        .step(1.0);

        classic_slider_row(
            text("Keep trades for"),
            slider_ui.into(),
            Some(
                text(format!("≈ {} min", retention_minutes.round() as u64))
                    .size(crate::style::text_size::EMPHASIS),
            ),
        )
    };

    let history_column = column![
        text("History").size(crate::style::text_size::SECTION),
        retention_slider
    ]
    .spacing(8);

    let content = split_column![
        display_options,
        history_column,
        row![
            space::horizontal(),
            sync_all_button(pane, VisualConfig::Ladder(cfg))
        ],
        ; spacing = 12, align_x = Alignment::Start
    ];

    cfg_view_container(320, content)
}

fn sync_all_button<'a>(pane: pane_grid::Pane, config: VisualConfig) -> Element<'a, Message> {
    tooltip(
        button("Sync all").on_press(Message::VisualConfigChanged(pane, config, true)),
        Some("Apply configuration to similar panes"),
        TooltipPosition::Top,
    )
}

pub mod study {
    use crate::{
        split_column,
        style::{self, Icon, icon_text},
    };
    use data::chart::heatmap::{CLEANUP_THRESHOLD, HeatmapStudy, ProfileKind};
    use data::chart::kline::FootprintStudy;
    use iced::{
        Element, padding,
        widget::{button, checkbox, column, container, row, slider, space, text},
    };

    #[derive(Debug, Clone, Copy)]
    pub enum StudyMessage {
        Footprint(Message<FootprintStudy>),
        Heatmap(Message<HeatmapStudy>),
    }

    pub trait Study: Sized + Copy + 'static {
        fn is_same_type(&self, other: &Self) -> bool;
        fn all() -> Vec<Self>;
        fn view_config<'a>(
            &self,
            basis: data::chart::Basis,
            on_change: impl Fn(Self) -> Message<Self> + Copy + 'a,
        ) -> Element<'a, Message<Self>>;
    }

    impl Study for FootprintStudy {
        fn is_same_type(&self, other: &Self) -> bool {
            std::mem::discriminant(self) == std::mem::discriminant(other)
        }

        fn all() -> Vec<Self> {
            FootprintStudy::ALL.to_vec()
        }

        fn view_config<'a>(
            &self,
            _basis: data::chart::Basis,
            on_change: impl Fn(Self) -> Message<Self> + Copy + 'a,
        ) -> Element<'a, Message<Self>> {
            match *self {
                FootprintStudy::NPoC { lookback } => {
                    let slider_ui = slider(10.0..=400.0, lookback as f32, move |new_value| {
                        on_change(FootprintStudy::NPoC {
                            lookback: new_value as usize,
                        })
                    })
                    .step(10.0);

                    column![text(format!("Lookback: {lookback} datapoints")), slider_ui]
                        .padding(8)
                        .spacing(4)
                        .into()
                }
                FootprintStudy::Imbalance {
                    threshold,
                    color_scale,
                    ignore_zeros,
                } => {
                    let qty_threshold = {
                        let info_text = text(format!("Ask:Bid threshold: {threshold}%"));

                        let threshold_slider =
                            slider(100.0..=800.0, threshold as f32, move |new_value| {
                                on_change(FootprintStudy::Imbalance {
                                    threshold: new_value as usize,
                                    color_scale,
                                    ignore_zeros,
                                })
                            })
                            .step(25.0);

                        column![info_text, threshold_slider,].padding(8).spacing(4)
                    };

                    let color_scaling = {
                        let color_scale_enabled = color_scale.is_some();
                        let color_scale_value = color_scale.unwrap_or(100);

                        let color_scale_checkbox = checkbox(color_scale_enabled)
                            .label("Dynamic color scaling")
                            .on_toggle(move |is_enabled| {
                                on_change(FootprintStudy::Imbalance {
                                    threshold,
                                    color_scale: if is_enabled {
                                        Some(color_scale_value)
                                    } else {
                                        None
                                    },
                                    ignore_zeros,
                                })
                            });

                        if color_scale_enabled {
                            let scaling_slider = column![
                                text(format!("Opaque color at: {color_scale_value}x")),
                                slider(50.0..=2000.0, color_scale_value as f32, move |new_value| {
                                    on_change(FootprintStudy::Imbalance {
                                        threshold,
                                        color_scale: Some(new_value as usize),
                                        ignore_zeros,
                                    })
                                })
                                .step(50.0)
                            ]
                            .spacing(2);

                            column![color_scale_checkbox, scaling_slider]
                                .padding(8)
                                .spacing(8)
                        } else {
                            column![color_scale_checkbox].padding(8)
                        }
                    };

                    let ignore_zeros_checkbox = {
                        let cbox = checkbox(ignore_zeros).label("Ignore zeros").on_toggle(
                            move |is_checked| {
                                on_change(FootprintStudy::Imbalance {
                                    threshold,
                                    color_scale,
                                    ignore_zeros: is_checked,
                                })
                            },
                        );

                        column![cbox].padding(8).spacing(4)
                    };

                    split_column![qty_threshold, color_scaling, ignore_zeros_checkbox]
                        .padding(4)
                        .into()
                }
            }
        }
    }

    impl Study for HeatmapStudy {
        fn is_same_type(&self, other: &Self) -> bool {
            std::mem::discriminant(self) == std::mem::discriminant(other)
        }

        fn all() -> Vec<Self> {
            HeatmapStudy::ALL.to_vec()
        }

        fn view_config<'a>(
            &self,
            basis: data::chart::Basis,
            on_change: impl Fn(Self) -> Message<Self> + Copy + 'a,
        ) -> Element<'a, Message<Self>> {
            let interval_ms = match basis {
                data::chart::Basis::Time(interval) => interval.to_milliseconds(),
                data::chart::Basis::Tick(_) => {
                    return iced::widget::center(text(
                        "Heatmap studies are not supported for tick-based charts",
                    ))
                    .into();
                }
            };

            match self {
                HeatmapStudy::VolumeProfile(kind) => match kind {
                    ProfileKind::FixedWindow(datapoint_count) => {
                        let duration_secs = (*datapoint_count as u64 * interval_ms) / 1000;
                        let min_range = CLEANUP_THRESHOLD / 20;

                        let duration_text = if duration_secs < 60 {
                            format!("{} seconds", duration_secs)
                        } else {
                            let minutes = duration_secs / 60;
                            let seconds = duration_secs % 60;
                            if seconds == 0 {
                                format!("{} minutes", minutes)
                            } else {
                                format!("{}m {}s", minutes, seconds)
                            }
                        };

                        let slider = slider(
                            min_range as f32..=CLEANUP_THRESHOLD as f32,
                            *datapoint_count as f32,
                            move |new_datapoint_count| {
                                on_change(HeatmapStudy::VolumeProfile(ProfileKind::FixedWindow(
                                    new_datapoint_count as usize,
                                )))
                            },
                        )
                        .step(40.0);

                        let switch_kind = button(text("Switch to visible range")).on_press(
                            on_change(HeatmapStudy::VolumeProfile(ProfileKind::VisibleRange)),
                        );

                        column![
                            row![space::horizontal(), switch_kind,],
                            text(format!(
                                "Window: {} datapoints ({})",
                                datapoint_count, duration_text
                            )),
                            slider,
                        ]
                        .padding(8)
                        .spacing(4)
                        .into()
                    }
                    ProfileKind::VisibleRange => {
                        let switch_kind = button(text("Switch to fixed window")).on_press(
                            on_change(HeatmapStudy::VolumeProfile(ProfileKind::FixedWindow(
                                CLEANUP_THRESHOLD / 5_usize,
                            ))),
                        );

                        column![row![space::horizontal(), switch_kind,],]
                            .padding(8)
                            .spacing(4)
                            .into()
                    }
                },
            }
        }
    }

    #[derive(Debug, Clone, Copy)]
    pub enum Message<S: Study> {
        CardToggled(S),
        StudyToggled(S, bool),
        StudyValueChanged(S),
    }

    pub enum Action<S: Study> {
        ToggleStudy(S, bool),
        ConfigureStudy(S),
    }

    pub struct Configurator<S: Study> {
        expanded_card: Option<S>,
    }

    impl<S: Study> Default for Configurator<S> {
        fn default() -> Self {
            Self {
                expanded_card: None,
            }
        }
    }

    impl<S: Study + ToString> Configurator<S> {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn update(&mut self, message: Message<S>) -> Option<Action<S>> {
            match message {
                Message::CardToggled(study) => {
                    let should_collapse = self
                        .expanded_card
                        .as_ref()
                        .is_some_and(|expanded| expanded.is_same_type(&study));

                    if should_collapse {
                        self.expanded_card = None;
                    } else {
                        self.expanded_card = Some(study);
                    }
                }
                Message::StudyToggled(study, is_checked) => {
                    return Some(Action::ToggleStudy(study, is_checked));
                }
                Message::StudyValueChanged(study) => {
                    return Some(Action::ConfigureStudy(study));
                }
            }

            None
        }

        pub fn view<'a>(
            &self,
            active_studies: &[S],
            basis: data::chart::Basis,
        ) -> Element<'a, Message<S>> {
            self.view_available(active_studies, basis, S::all())
        }

        pub fn view_available<'a>(
            &self,
            active_studies: &[S],
            basis: data::chart::Basis,
            available_studies: Vec<S>,
        ) -> Element<'a, Message<S>> {
            let mut content = column![].spacing(4);

            for available_study in available_studies {
                content =
                    content.push(self.create_study_row(available_study, active_studies, basis));
            }

            content.into()
        }

        fn create_study_row<'a>(
            &self,
            study: S,
            active_studies: &[S],
            basis: data::chart::Basis,
        ) -> Element<'a, Message<S>> {
            let (is_selected, study_config) = {
                let mut is_selected = false;
                let mut study_config = None;

                for s in active_studies {
                    if s.is_same_type(&study) {
                        is_selected = true;
                        study_config = Some(*s);
                        break;
                    }
                }
                (is_selected, study_config)
            };

            let checkbox = checkbox(is_selected)
                .label(study_config.map_or(study.to_string(), |s| s.to_string()))
                .on_toggle(move |checked| Message::StudyToggled(study, checked));

            let mut checkbox_row = row![checkbox, space::horizontal()]
                .height(36)
                .align_y(iced::Alignment::Center)
                .padding(padding::left(8).right(4))
                .spacing(4);

            let is_expanded = self
                .expanded_card
                .as_ref()
                .is_some_and(|expanded| expanded.is_same_type(&study));

            if is_selected {
                checkbox_row = checkbox_row.push(
                    button(icon_text(Icon::Cog, 12))
                        .on_press(Message::CardToggled(study))
                        .style(move |theme, status| {
                            style::button::transparent(theme, status, is_expanded)
                        }),
                );
            }

            let mut column = column![checkbox_row];

            if is_expanded && let Some(config) = study_config {
                column = column.push(config.view_config(basis, Message::StudyValueChanged));
            }

            container(column).style(style::modal_container).into()
        }
    }
}
