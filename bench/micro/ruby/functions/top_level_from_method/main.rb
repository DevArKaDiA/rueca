def format_name(s)
  s.strip
end

class Card
  def label
    format_name(" x ")
  end
end
