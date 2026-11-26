class Printer
  def print_doc(doc)
    doc.render
  end
end

class Page
  def render
    1
  end
end
