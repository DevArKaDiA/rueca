# typed: true
class Document
  def parse
    1
  end
end

class Indexer
  #: (Document doc) -> void
  def index(doc)
    doc.parse
  end
end
