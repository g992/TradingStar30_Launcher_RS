use crate::Message; // Импортируем типы из корневого модуля
use iced::{
    advanced::subscription::{EventStream, Recipe},
    futures::stream::{BoxStream, StreamExt},
};
// Добавляем нужные use для Hasher и Hash
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command as TokioCommand};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

// --- Управление процессом ---

// Функция для принудительного завершения процесса по PID
pub async fn kill_process(pid: u32) -> Result<(), String> {
    println!("[kill_process] Попытка завершить процесс с PID: {}", pid);

    #[cfg(unix)]
    {
        println!("[kill_process] Выполнение команды: kill {}", pid);
        // Используем TokioCommand для выполнения системной команды
        let kill_cmd = TokioCommand::new("kill")
            .arg(pid.to_string())
            .output() // Получаем вывод команды
            .await;
        match kill_cmd {
            Ok(output) => {
                println!("[kill_process] Статус kill: {}", output.status);
                // Логируем stdout и stderr команды kill
                if !output.stdout.is_empty() {
                    println!(
                        "[kill_process] kill stdout: {}",
                        String::from_utf8_lossy(&output.stdout)
                    );
                }
                if !output.stderr.is_empty() {
                    println!(
                        "[kill_process] kill stderr: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
                // Проверяем успешность выполнения команды
                if output.status.success() {
                    println!(
                        "[kill_process] Команда kill успешно завершена для PID: {}",
                        pid
                    );
                    Ok(())
                } else {
                    // Возвращаем ошибку, если команда завершилась неудачно
                    Err(format!(
                        "Команда kill для PID {} завершилась с кодом: {}. Stderr: {}",
                        pid,
                        output.status,
                        String::from_utf8_lossy(&output.stderr)
                    ))
                }
            }
            Err(e) => {
                // Обрабатываем ошибку выполнения самой команды kill
                let error_msg = format!("Ошибка выполнения команды kill для PID {}: {}", pid, e);
                println!("[kill_process] {}", error_msg);
                Err(error_msg)
            }
        }
    }

    #[cfg(windows)]
    {
        println!(
            "[kill_process] Выполнение команды: taskkill /F /PID {}",
            pid
        );
        // Используем taskkill для Windows
        let kill_cmd = TokioCommand::new("taskkill")
            .arg("/F") // Принудительное завершение
            .arg("/PID") // Указываем PID
            .arg(pid.to_string())
            .output()
            .await;

        match kill_cmd {
            Ok(output) => {
                println!("[kill_process] Статус taskkill: {}", output.status);
                if !output.stdout.is_empty() {
                    println!(
                        "[kill_process] taskkill stdout: {}",
                        String::from_utf8_lossy(&output.stdout)
                    );
                }
                if !output.stderr.is_empty() {
                    println!(
                        "[kill_process] taskkill stderr: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
                if output.status.success() {
                    // На Windows taskkill может завершиться успешно, даже если процесс уже мертв.
                    // Проверяем stdout для большей уверенности (хотя это не идеально).
                    let stdout = String::from_utf8_lossy(&output.stdout).to_lowercase();
                    if stdout.contains(&format!("pid {} ", pid)) || stdout.contains("success") {
                        println!(
                            "[kill_process] Команда taskkill успешно завершена для PID: {}",
                            pid
                        );
                        Ok(())
                    } else {
                        println!("[kill_process] taskkill stdout не содержит подтверждения успеха для PID {}. Возможно, процесс уже был завершен.", pid);
                        // Считаем успехом, т.к. цель - отсутствие процесса
                        Ok(())
                    }
                } else {
                    Err(format!(
                        "Команда taskkill для PID {} завершилась с кодом: {}. Stderr: {}",
                        pid,
                        output.status,
                        String::from_utf8_lossy(&output.stderr)
                    ))
                }
            }
            Err(e) => {
                let error_msg =
                    format!("Ошибка выполнения команды taskkill для PID {}: {}", pid, e);
                println!("[kill_process] {}", error_msg);
                Err(error_msg)
            }
        }
    }

    #[cfg(not(any(unix, windows)))]
    {
        // Заглушка для неподдерживаемых ОС
        let error_msg = "Остановка процесса не поддерживается на этой ОС.".to_string();
        println!("[kill_process] {}", error_msg);
        Err(error_msg)
    }
}

// --- ProcessListener Recipe для подписки Iced ---
#[derive(Debug)]
pub struct ProcessListener {
    // Структура для хранения данных подписки
    id: u64,         // Уникальный идентификатор подписки
    path: PathBuf,   // Путь к исполняемому файлу
    api_key: String, // Ключ API
}
impl ProcessListener {
    // Публичный конструктор
    pub fn new(id: u64, path: PathBuf, api_key: String) -> Self {
        Self { id, path, api_key }
    }
}
// Реализация Recipe для интеграции с Iced
impl Recipe for ProcessListener {
    type Output = Message; // Тип сообщений, которые генерирует подписка

    // Хеширование для идентификации подписки
    fn hash(&self, state: &mut iced::advanced::Hasher) {
        // Используем TypeId и id для уникальности
        std::any::TypeId::of::<Self>().hash(state);
        self.id.hash(state);
    }

    // Создание потока событий
    fn stream(self: Box<Self>, _input: EventStream) -> BoxStream<'static, Self::Output> {
        // Создаем MPSC канал для передачи сообщений из асинхронных задач в Iced
        let (sender, receiver) = mpsc::channel(100);

        let path = self.path;
        let api_key = self.api_key;

        // Запускаем главную асинхронную задачу
        tokio::spawn(async move {
            let mut child: Child;
            let actual_pid: u32;
            // Запускаем дочерний процесс
            match TokioCommand::new(&path)
                .arg("-k") // Передаем ключ API как аргумент
                .arg(&api_key)
                .stdout(Stdio::piped()) // Перехватываем stdout
                .stderr(Stdio::piped()) // Перехватываем stderr
                .kill_on_drop(true) // Завершать процесс, если лаунчер упадет
                .spawn()
            {
                Ok(spawned_child) => {
                    child = spawned_child;
                    // Получаем PID запущенного процесса
                    if let Some(pid) = child.id() {
                        actual_pid = pid;
                        // Отправляем PID в основной поток Iced
                        if sender
                            .send(Message::ProcessActualPid(actual_pid))
                            .await
                            .is_err()
                        {
                            eprintln!("[Recipe] Failed to send actual PID");
                            return; // Завершаем задачу, если канал закрыт
                        }
                    } else {
                        // Обрабатываем ошибку получения PID
                        let _ = sender
                            .send(Message::ProcessError(
                                "Не удалось получить PID запущенного процесса.".to_string(),
                            ))
                            .await;
                        return;
                    }
                }
                Err(e) => {
                    // Обрабатываем ошибку запуска процесса
                    let _ = sender
                        .send(Message::ProcessError(format!(
                            "Ошибка запуска процесса {:?}: {}",
                            path, e
                        )))
                        .await;
                    return;
                }
            }

            // Получаем пайпы stdout и stderr
            let stdout = child.stdout.take().expect("stdout not captured");
            let stderr = child.stderr.take().expect("stderr not captured");

            // Запускаем задачу для чтения stdout
            let sender_stdout = sender.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(stdout).lines();
                // Читаем строки и отправляем их как сообщения ProcessOutput
                while let Ok(Some(line)) = reader.next_line().await {
                    if sender_stdout
                        .send(Message::ProcessOutput(line))
                        .await
                        .is_err()
                    {
                        break; // Канал закрыт
                    }
                }
                println!("[Recipe] Stdout reader finished.");
            });

            // Запускаем задачу для чтения stderr
            let sender_stderr = sender.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(stderr).lines();
                // Читаем строки и отправляем их как сообщения ProcessOutput с префиксом
                while let Ok(Some(line)) = reader.next_line().await {
                    if sender_stderr
                        .send(Message::ProcessOutput(format!("STDERR: {}", line)))
                        .await
                        .is_err()
                    {
                        break; // Канал закрыт
                    }
                }
                println!("[Recipe] Stderr reader finished.");
            });

            // Запускаем задачу для ожидания завершения процесса
            let sender_termination = sender;
            tokio::spawn(async move {
                // Ожидаем завершения дочернего процесса
                let message = match child.wait().await {
                    Ok(status) => Message::ProcessTerminated(status.code().unwrap_or(-1)), // Отправляем код завершения
                    Err(e) => Message::ProcessError(format!(
                        // Отправляем ошибку ожидания
                        "Ошибка ожидания процесса PID {}: {}",
                        actual_pid, e
                    )),
                };
                // Отправляем сообщение о завершении/ошибке
                let _ = sender_termination.send(message).await;
                println!("[Recipe] Process termination listener finished.");
            });
        });

        // Оборачиваем ресивер канала в BoxStream для Iced
        ReceiverStream::new(receiver).boxed()
    }
}
