class Mailer
  def deliver
    compose
    send_now
  end

  private

  def compose
    1
  end

  def send_now
    2
  end
end
