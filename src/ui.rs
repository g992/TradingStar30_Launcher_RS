use crate::settings::AppSettings; // Используем AppSettings напрямую
use crate::Message; // Импортируем Message из корневого модуля
use ansi_parser::{AnsiParser, AnsiSequence, Output};
use iced::widget::{
    button, column, container, row, scrollable, text, text_input, Button, Column, Container, Row,
    Scrollable, Space, Text, TextInput,
};
use iced::{theme, Alignment, Background, Border, Color, Element, Font, Length, Theme};
use std::collections::VecDeque;

// --- Константы для UI ---
pub const MAX_LOG_LINES: usize = 500; // Максимальное количество строк лога
pub const BUTTON_TEXT_COLOR: Color = Color::WHITE; // Цвет текста на кнопках

// --- Структура для сегмента ANSI ---
// Представляет собой часть строки лога с определенным цветом
#[derive(Debug, Clone, PartialEq)]
pub struct AnsiSegment {
    pub text: String,         // Текст сегмента
    pub color: Option<Color>, // Цвет текста (None для цвета по умолчанию)
}

// --- Логика обработки и добавления логов ---

// Вспомогательная функция для конвертации кода цвета ANSI в цвет Iced
fn ansi_to_iced_color(code: u8) -> Color {
    // https://en.wikipedia.org/wiki/ANSI_escape_code#3-bit_and_4-bit
    match code {
        // Стандартные цвета (30-37)
        30 => Color::from_rgb8(0x01, 0x01, 0x01), // Почти черный, чтобы отличался от фона
        31 => Color::from_rgb8(0xAA, 0x00, 0x00), // Red
        32 => Color::from_rgb8(0x00, 0xAA, 0x00), // Green
        33 => Color::from_rgb8(0xAA, 0xAA, 0x00), // Yellow
        34 => Color::from_rgb8(0x00, 0x00, 0xAA), // Blue
        35 => Color::from_rgb8(0xAA, 0x00, 0xAA), // Magenta
        36 => Color::from_rgb8(0x00, 0xAA, 0xAA), // Cyan
        37 => Color::from_rgb8(0xAA, 0xAA, 0xAA), // White (Gray)
        // Яркие цвета (90-97)
        90 => Color::from_rgb8(0x55, 0x55, 0x55), // Bright Black (Dark Gray)
        91 => Color::from_rgb8(0xFF, 0x55, 0x55), // Bright Red
        92 => Color::from_rgb8(0x55, 0xFF, 0x55), // Bright Green
        93 => Color::from_rgb8(0xFF, 0xFF, 0x55), // Bright Yellow
        94 => Color::from_rgb8(0x55, 0x55, 0xFF), // Bright Blue
        95 => Color::from_rgb8(0xFF, 0x55, 0xFF), // Bright Magenta
        96 => Color::from_rgb8(0x55, 0xFF, 0xFF), // Bright Cyan
        97 => Color::from_rgb8(0xFF, 0xFF, 0xFF), // Bright White
        // Коды сброса (0, 39, 49) интерпретируем как цвет по умолчанию (белый для темной темы)
        0 | 39 | 49 => Color::WHITE,
        // Остальные коды пока игнорируем
        _ => Color::WHITE,
    }
}

// Реализация добавления и парсинга лога
pub fn add_log_impl(logs: &mut VecDeque<Vec<AnsiSegment>>, message: String) {
    let mut segments = Vec::new(); // Вектор для хранения сегментов текущей строки
    let mut current_color: Option<Color> = None; // Текущий цвет текста
    let mut current_text = String::new(); // Текущий накапливаемый текст

    // Парсим строку с помощью ansi_parser
    for block in message.ansi_parse() {
        match block {
            // Если это текстовый блок, добавляем его к текущему тексту
            Output::TextBlock(text) => {
                current_text.push_str(text);
            }
            // Если это управляющая последовательность ANSI
            Output::Escape(sequence) => {
                // Нас интересует только SetGraphicsMode (SGR) для установки стилей/цветов
                if let AnsiSequence::SetGraphicsMode(codes) = sequence {
                    // Перед изменением цвета сохраняем предыдущий сегмент, если он был
                    if !current_text.is_empty() {
                        segments.push(AnsiSegment {
                            text: std::mem::take(&mut current_text),
                            color: current_color,
                        });
                    }

                    // Обрабатываем коды SGR
                    if codes.is_empty() {
                        // `ESC[m` (пустой код) - сброс всех атрибутов
                        current_color = None;
                    } else {
                        for code in codes {
                            match code {
                                // Код 0 - сброс
                                0 => current_color = None,
                                // Коды цвета переднего плана (30-37, 90-97)
                                c @ 30..=37 | c @ 90..=97 => {
                                    current_color = Some(ansi_to_iced_color(c));
                                }
                                // Код 39 - сброс цвета переднего плана по умолчанию
                                39 => current_color = None,
                                // Пока игнорируем цвета фона (40-47, 100-107) и другие атрибуты (жирность, курсив и т.д.)
                                _ => {}
                            }
                        }
                    }
                }
                // Игнорируем другие Escape последовательности (перемещение курсора и т.д.)
            }
        }
    }

    // Добавляем последний сегмент текста, если он остался
    if !current_text.is_empty() {
        segments.push(AnsiSegment {
            text: current_text,
            color: current_color,
        });
    }

    // Удаляем пустые сегменты, которые могли образоваться (например, из-за `ESC[mESC[31m`)
    segments.retain(|seg| !seg.text.is_empty());

    // Добавляем распарсенную строку в очередь логов, если она не пустая
    if !segments.is_empty() {
        // Ограничиваем максимальное количество строк
        if logs.len() >= MAX_LOG_LINES {
            logs.pop_front();
        }
        logs.push_back(segments);
    }
}

// --- Функции отрисовки View ---

// Отрисовка основного экрана приложения
pub fn view_main(
    is_running: bool,                  // Запущен ли процесс?
    logs: &VecDeque<Vec<AnsiSegment>>, // Ссылка на логи
    settings: &AppSettings,            // Ссылка на настройки (для проверки кнопки Start)
) -> Element<'static, Message> {
    // 'static lifetime необходим для элементов Iced

    // Верхняя панель
    let top_bar_content = row![
        text("TradingStar 3 Launcher").size(20),
        Space::with_width(Length::Fill), // Растягиваем пространство
        // Кнопка "Настройки"
        button(text("Настройки"))
            .padding(10)
            .style(theme::Button::Custom(Box::new(DefaultButtonStyle))) // Используем стиль
            .on_press(Message::SettingsButtonPressed) // Сообщение при нажатии
    ]
    .spacing(20)
    .align_items(Alignment::Center)
    .padding(10);

    // Контейнер для верхней панели со стилем
    let top_bar_container = container(top_bar_content)
        .width(Length::Fill)
        .style(theme::Container::Custom(Box::new(TopBarStyle))); // Используем стиль

    // Кнопка "Запуск/Остановка"
    let control_button_element: Element<'static, Message> = if is_running {
        button(text("Остановка программы"))
            .padding(10)
            .style(theme::Button::Custom(Box::new(StopButtonStyle)))
            .on_press(Message::StopButtonPressed)
            .into()
    } else {
        let start_button = button(text("Запуск программы")).padding(10);
        if settings.executable_path.is_some() && !settings.api_key.is_empty() {
            start_button
                .style(theme::Button::Custom(Box::new(StartButtonStyle)))
                .on_press(Message::StartButtonPressed)
                .into()
        } else {
            start_button
                .style(theme::Button::Custom(Box::new(DisabledButtonStyle)))
                .into()
        }
    };

    // Кнопка Копировать лог
    let copy_log_button: Element<'static, Message> = button(text("Копировать лог"))
        .padding(10)
        .style(theme::Button::Custom(Box::new(DefaultButtonStyle)))
        .on_press(Message::CopyLogsPressed)
        .into();

    // Строка с кнопками управления
    let control_row = row![
        copy_log_button,
        Space::with_width(Length::Fill),
        control_button_element
    ]
    .spacing(10) // Добавим немного места между кнопками
    .padding(10);

    // Формирование вида логов
    let log_lines: Column<'static, Message> = logs.iter().rev().fold(
        column![]
            .spacing(2) // <-- Возвращаем небольшой spacing для колонки
            .padding(10),
        |column, line_segments| {
            let log_row: Row<'static, Message> =
                line_segments
                    .iter()
                    .fold(row![].spacing(0), |row_acc, segment| {
                        let segment_text: Text<'static> = text(&segment.text)
                            .size(12)
                            .font(Font::MONOSPACE)
                            .style(segment.color.unwrap_or(Color::WHITE));
                        row_acc.push(segment_text)
                    });
            // Убираем контейнер, добавляем Row напрямую
            // let line_container = container(log_row)
            //                         .width(Length::Fill)
            //                         .style(theme::Container::Custom(Box::new(LogLineStyle)));
            // column.push(line_container)
            column.push(log_row) // <-- Добавляем Row напрямую
        },
    );

    // Оборачиваем колонку логов в Scrollable
    let log_view: Scrollable<'static, Message> = scrollable(log_lines)
        .height(Length::Fill)
        .width(Length::Fill);

    // Собираем главный экран
    column![top_bar_container, control_row, log_view]
        .spacing(10)
        .padding(0)
        .into()
}

// Отрисовка экрана настроек
pub fn view_settings(settings: &AppSettings) -> Element<'static, Message> {
    // 'static lifetime необходим для элементов Iced

    // Отображение выбранного пути
    let path_display = match &settings.executable_path {
        Some(path) => path.display().to_string(),
        None => "Путь не выбран".to_string(),
    };

    // Формируем колонку с элементами настроек
    column![
        text("Настройки").size(24),
        Space::with_height(20), // Отступ
        text("Путь к исполняемому файлу:"),
        // Строка с путем и кнопкой выбора
        row![
            text(path_display).width(Length::Fill), // Текст пути растягивается
            button(text("Выбрать..."))
                .padding(5)
                .style(theme::Button::Custom(Box::new(DefaultButtonStyle))) // Используем стиль
                .on_press(Message::SelectExecutablePath)  // Сообщение при нажатии
        ]
        .spacing(10)
        .align_items(Alignment::Center),
        Space::with_height(15), // Отступ
        text("Ключ API (параметр -k):"),
        // Поле ввода ключа API
        text_input("Введите ваш API ключ...", &settings.api_key)
            .on_input(Message::ApiKeyChanged) // Сообщение при изменении
            .padding(10),
        Space::with_height(Length::Fill), // Растягиваем пространство до низа
        // Кнопка "Закрыть настройки"
        button(text("Закрыть настройки"))
            .padding(10)
            .style(theme::Button::Custom(Box::new(DefaultButtonStyle))) // Используем стиль
            .on_press(Message::CloseSettingsPressed) // Сообщение при нажатии
    ]
    .padding(20) // Внутренние отступы колонки
    .spacing(10) // Пространство между элементами колонки
    .max_width(600) // Ограничиваем максимальную ширину
    .into() // Преобразуем в Element
}

// --- Стили виджетов ---

// Стиль для верхней панели
struct TopBarStyle;
impl container::StyleSheet for TopBarStyle {
    type Style = Theme;
    fn appearance(&self, _style: &Self::Style) -> container::Appearance {
        container::Appearance {
            background: Some(Color::from_rgb8(0x00, 0x7B, 0xFF).into()), // Синий фон
            text_color: Some(Color::WHITE),                              // Белый текст по умолчанию
            ..Default::default()
        }
    }
}

// Общий стиль для кнопок по умолчанию (синий)
struct DefaultButtonStyle;
impl button::StyleSheet for DefaultButtonStyle {
    type Style = Theme;
    fn active(&self, _style: &Self::Style) -> button::Appearance {
        button::Appearance {
            background: Some(Background::Color(Color::from_rgb8(0x00, 0x7B, 0xFF))), // Синий
            text_color: BUTTON_TEXT_COLOR, // Белый текст (из константы)
            border: Border {
                radius: 4.0.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }
    // Стиль при наведении
    fn hovered(&self, style: &Self::Style) -> button::Appearance {
        let active = self.active(style);
        button::Appearance {
            background: Some(Background::Color(Color::from_rgb8(0x00, 0x56, 0xB3))), // Темнее синий
            ..active // Остальные свойства как у active
        }
    }
}

// Стиль для кнопки "Старт" (зеленый)
struct StartButtonStyle;
impl button::StyleSheet for StartButtonStyle {
    type Style = Theme;
    fn active(&self, _style: &Self::Style) -> button::Appearance {
        button::Appearance {
            background: Some(Background::Color(Color::from_rgb8(0x28, 0xA7, 0x45))), // Зеленый
            text_color: BUTTON_TEXT_COLOR,
            border: Border {
                radius: 4.0.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }
    // Стиль при наведении
    fn hovered(&self, style: &Self::Style) -> button::Appearance {
        let active = self.active(style);
        button::Appearance {
            background: Some(Background::Color(Color::from_rgb8(0x21, 0x88, 0x38))), // Темнее зеленый
            ..active
        }
    }
}

// Стиль для кнопки "Стоп" (красный)
struct StopButtonStyle;
impl button::StyleSheet for StopButtonStyle {
    type Style = Theme;
    fn active(&self, _style: &Self::Style) -> button::Appearance {
        button::Appearance {
            background: Some(Background::Color(Color::from_rgb8(0xDC, 0x35, 0x45))), // Красный
            text_color: BUTTON_TEXT_COLOR,
            border: Border {
                radius: 4.0.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }
    // Стиль при наведении
    fn hovered(&self, style: &Self::Style) -> button::Appearance {
        let active = self.active(style);
        button::Appearance {
            background: Some(Background::Color(Color::from_rgb8(0xC8, 0x23, 0x33))), // Темнее красный
            ..active
        }
    }
}

// Стиль для неактивной кнопки "Старт" (серый)
struct DisabledButtonStyle;
impl button::StyleSheet for DisabledButtonStyle {
    type Style = Theme;
    fn active(&self, _style: &Self::Style) -> button::Appearance {
        button::Appearance {
            background: Some(Background::Color(Color::from_rgb8(0x6C, 0x75, 0x7D))), // Серый
            text_color: Color::from_rgb8(0xCC, 0xCC, 0xCC), // Светло-серый текст
            border: Border {
                radius: 4.0.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }
    // Неактивная кнопка не меняет вид при наведении
    fn hovered(&self, style: &Self::Style) -> button::Appearance {
        self.active(style)
    }
}
